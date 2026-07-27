//! Adapts a block world to the physics engine's [`CollisionView`], so the shell
//! drives the **real** bit-exact movement code in `lodestone-physics` rather
//! than any ad-hoc integrator.
//!
//! Two adapters live here, one per world source:
//! - [`WorldCollision`] over the offline demo [`lodestone_world::World`], keyed
//!   by the demo palette.
//! - [`LiveCollision`] over an owned snapshot of the **live server world**, keyed
//!   by vanilla block-state ids and the vanilla classifier.
//!
//! Both use the *same* deliberately-coarse mapping — every full opaque cube is a
//! unit-cube collider, water reports `is_water` (so the engine runs its swim
//! path), everything else is empty — so switching between them changes only
//! *where blocks come from*, never how collision resolves. The bit-exact
//! movement in `lodestone-physics` is untouched.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use lodestone_physics::{Aabb, CollisionView};
use lodestone_render::{BlockAtlas, BlockClassifier};
use lodestone_world::{ChunkPos, ChunkSection, World};

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

/// A [`CollisionView`] over the **live server world**, keyed by vanilla block
/// state ids.
///
/// This is the multiplayer companion to [`WorldCollision`]: it makes the shell's
/// physics walk on the *server's* terrain instead of the offline demo world.
/// Crucially it changes **where blocks come from, not how collision resolves** —
/// it fills the exact same [`CollisionView`] hooks the demo adapter does (full
/// cube for a solid block, `is_water` for water, defaults for the rest), so the
/// bit-exact movement in `lodestone-physics` is untouched. The two adapters are
/// deliberately the same coarseness: a full opaque cube collides, everything else
/// (air, water, foliage, and — a known v1 limitation — partial blocks like slabs
/// and stairs) does not.
///
/// Solidity is read from the same vanilla classifier the live mesher uses: a
/// block whose baked model is a full six-faced cube reports
/// [`Cell::occludes`](lodestone_render::Cell::occludes), which is exactly the set
/// that should stop a player. Water and air classify as non-occluding, so they
/// fall through to `is_water`/empty automatically.
///
/// The sections are an **owned snapshot** — a map of `Arc<ChunkSection>` pulled
/// from the client-owned world under a single lock (see
/// [`crate::net::NetClient::sections_at`]) — so no world lock is held while
/// physics queries it, and the many per-tick block lookups touch only local
/// memory.
#[derive(Debug)]
pub struct LiveCollision {
    /// `(chunk-x, chunk-z, section-index)` → owned block section. A missing key
    /// (unloaded or all-air section) reads as air.
    sections: HashMap<(i32, i32, usize), Arc<ChunkSection>>,
    /// World-space bottom of the dimension (overworld `-64`).
    min_y: i32,
    /// Number of 16-block sections stacked in a column.
    section_count: usize,
    /// The vanilla classifier: `classify(id).occludes` is the solidity oracle.
    atlas: Arc<BlockAtlas>,
    /// Vanilla water state ids, for the `is_water` swim hook. Non-solid already
    /// (water never occludes), so this only drives buoyancy, not collision.
    water: Arc<HashSet<u32>>,
}

impl LiveCollision {
    /// Build a view from a pre-fetched section snapshot and the dimension geometry.
    #[must_use]
    pub fn new(
        sections: HashMap<(i32, i32, usize), Arc<ChunkSection>>,
        min_y: i32,
        section_count: usize,
        atlas: Arc<BlockAtlas>,
        water: Arc<HashSet<u32>>,
    ) -> Self {
        Self {
            sections,
            min_y,
            section_count,
            atlas,
            water,
        }
    }

    /// Vanilla block-state id at world coordinates, or `0` (`minecraft:air`)
    /// outside the snapshot / world.
    #[must_use]
    pub fn block_at(&self, x: i32, y: i32, z: i32) -> u32 {
        if y < self.min_y || y >= self.min_y + (self.section_count as i32) * 16 {
            return 0;
        }
        let si = ((y - self.min_y) / 16) as usize;
        let key = (x.div_euclid(16), z.div_euclid(16), si);
        let Some(section) = self.sections.get(&key) else {
            return 0;
        };
        let ly = (y - self.min_y).rem_euclid(16) as usize;
        section.get_block(x.rem_euclid(16) as usize, ly, z.rem_euclid(16) as usize)
    }

    /// Whether the block at these coordinates is a full-cube collider (a full
    /// six-faced opaque cube per the vanilla classifier). Air, water and foliage
    /// classify as non-occluding and so do not collide.
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        self.atlas.classify(self.block_at(x, y, z), 0, 0).occludes
    }
}

impl CollisionView for LiveCollision {
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
        self.water.contains(&self.block_at(x, y, z))
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
