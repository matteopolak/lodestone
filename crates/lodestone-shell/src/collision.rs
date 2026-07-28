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
//! unit-cube collider, a fluid cell reports `is_water`/`is_lava` (so the engine
//! runs its swim path), everything else is empty — so switching between them
//! changes only *where blocks come from*, never how collision resolves. The
//! bit-exact movement in `lodestone-physics` is untouched.
//!
//! # Fluids have one classifier, not one per consumer
//!
//! Neither adapter decides what water *is*. Both call
//! [`crate::blocks::vanilla_fluid`] / [`crate::blocks::demo_fluid`], and the
//! vanilla one delegates to [`BlockModels::fluid`](lodestone_render::BlockModels::fluid)
//! — the classification the mesher already bakes the water surface from. That is
//! deliberate: `is_water` gates swimming, and the [`FluidState`] it feeds
//! (`lodestone_physics::compute_fluid_state`) gates the submerged fog, the
//! underwater overlay and the `ambient.underwater.*` sounds. This adapter matching
//! an exact `minecraft:water` id — while the mesher drew water for every
//! `waterlogged=true` block and for kelp/seagrass/bubble columns — is precisely
//! how a player ended up standing *visibly* underwater, unable to swim, with clear
//! sky fog. One question, one answer.
//!
//! [`FluidState`]: lodestone_physics::FluidState

use std::collections::HashMap;
use std::sync::Arc;

use lodestone_physics::{Aabb, CollisionView};
use lodestone_render::{BlockAtlas, BlockClassifier, FluidKind};
use lodestone_world::{ChunkPos, ChunkSection, World};

use crate::blocks::{demo_fluid, id, vanilla_fluid};

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
        demo_fluid(self.block_at(x, y, z)) == Some(FluidKind::Water)
    }

    fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
        demo_fluid(self.block_at(x, y, z)) == Some(FluidKind::Lava)
    }
}

/// A [`CollisionView`] over the **live server world**, keyed by vanilla block
/// state ids.
///
/// This is the multiplayer companion to [`WorldCollision`]: it makes the shell's
/// physics walk on the *server's* terrain instead of the offline demo world.
/// Crucially it changes **where blocks come from, not how collision resolves** —
/// it fills the exact same [`CollisionView`] hooks the demo adapter does (full
/// cube for a solid block, `is_water`/`is_lava` for fluids, defaults for the
/// rest), so the bit-exact movement in `lodestone-physics` is untouched. The two
/// adapters are deliberately the same coarseness: a full opaque cube collides,
/// everything else (air, water, foliage, and — a known v1 limitation — partial
/// blocks like slabs and stairs) does not.
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
    /// The vanilla classifier: `classify(id).occludes` is the solidity oracle,
    /// and its attached [`BlockModels`](lodestone_render::BlockModels) is the
    /// fluid oracle behind [`is_water`](CollisionView::is_water) /
    /// [`is_lava`](CollisionView::is_lava) — see [`vanilla_fluid`].
    atlas: Arc<BlockAtlas>,
}

impl LiveCollision {
    /// Build a view from a pre-fetched section snapshot and the dimension geometry.
    #[must_use]
    pub fn new(
        sections: HashMap<(i32, i32, usize), Arc<ChunkSection>>,
        min_y: i32,
        section_count: usize,
        atlas: Arc<BlockAtlas>,
    ) -> Self {
        Self {
            sections,
            min_y,
            section_count,
            atlas,
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

    /// The fluid occupying these coordinates, from the *same* classification the
    /// mesher draws the water surface with ([`vanilla_fluid`]). This is the one
    /// place the live view answers "is there fluid here"; both
    /// [`is_water`](CollisionView::is_water) and [`is_lava`](CollisionView::is_lava)
    /// are thin reads of it, so they cannot disagree about a waterlogged block.
    #[must_use]
    fn fluid_kind(&self, x: i32, y: i32, z: i32) -> Option<FluidKind> {
        vanilla_fluid(&self.atlas, self.block_at(x, y, z))
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
        self.fluid_kind(x, y, z) == Some(FluidKind::Water)
    }

    fn is_lava(&self, x: i32, y: i32, z: i32) -> bool {
        self.fluid_kind(x, y, z) == Some(FluidKind::Lava)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_physics::{
        MovementInput, PhysicsProfile, PlayerState, Vec3d, compute_fluid_state, tick,
    };
    use lodestone_world::PaletteKind;

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

    /// The vanilla atlas (with baked models attached), or a loud failure naming
    /// the fix. Every test below needs the real state ids — a waterlogged stair
    /// or a kelp plant only exists in the vanilla id space.
    fn vanilla_atlas() -> Arc<BlockAtlas> {
        let resources = crate::resources::BlockResources::load(true);
        resources.vanilla_atlas.expect(
            "vanilla assets did not load; set LODESTONE_ASSETS to a pack root with \
             client.jar + generated/reports/blocks.json",
        )
    }

    /// Resolve a full vanilla block-state string, failing loudly (rather than
    /// silently testing air) when the name or its property set is wrong.
    fn state_id(atlas: &BlockAtlas, name: &str) -> u32 {
        atlas
            .state_id_of(name)
            .unwrap_or_else(|| panic!("no such block state: {name}"))
    }

    /// A one-section live view (chunk `0,0`, `min_y = 0`) whose cells at
    /// `y_range` hold `state` and whose remaining cells are air.
    fn live_column(
        atlas: Arc<BlockAtlas>,
        state: u32,
        y_range: std::ops::RangeInclusive<usize>,
    ) -> LiveCollision {
        // 20 direct bits, comfortably above the ~28k vanilla state ids, so no id
        // in this fixture can be truncated by the container's packing.
        let mut section = ChunkSection::new(
            PaletteKind::block_states_with_direct_bits(20),
            PaletteKind::biomes(),
            0,
            0,
        );
        for y in y_range {
            for x in 0..16 {
                for z in 0..16 {
                    section.set_block(x, y, z, state);
                }
            }
        }
        let mut sections = HashMap::new();
        sections.insert((0, 0, 0), Arc::new(section));
        LiveCollision::new(sections, 0, 1, atlas)
    }

    /// The player's fluid summary standing feet-first at `feet_y` in the column,
    /// with an explicit pose eye height.
    fn fluid_state_at(
        view: &LiveCollision,
        feet_y: f64,
        eye_height: f32,
    ) -> lodestone_physics::FluidState {
        let profile = PhysicsProfile::mc_1_21();
        let position = Vec3d::new(0.5, feet_y, 0.5);
        let player = PlayerState::at(position, 0.0).with_eye_height(eye_height);
        compute_fluid_state(player.bounding_box(&profile), position, eye_height, view)
    }

    /// The defect: a **waterlogged** block and the plants that hardcode a water
    /// `getFluidState` carry a water source in vanilla, so an eye inside one is
    /// under water — swimming, submerged fog, the overlay and the ambient sounds
    /// all hang off this one answer. The mesher has always classified these
    /// correctly (`BlockModels::fluid`); physics used to match an exact water id
    /// and saw none of them.
    ///
    /// The `waterlogged=false` stair and the land plant are the **controls**: if
    /// they submerged too, the classifier would be saying "yes" to everything and
    /// the positive assertions would prove nothing.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn waterlogged_blocks_and_underwater_plants_submerge_the_eye() {
        let atlas = vanilla_atlas();

        let wet = [
            "minecraft:oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=true]",
            "minecraft:oak_slab[type=bottom,waterlogged=true]",
            "minecraft:kelp_plant",
            "minecraft:kelp[age=0]",
            "minecraft:seagrass",
            "minecraft:tall_seagrass[half=lower]",
            "minecraft:water[level=0]",
        ];
        for name in wet {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 0..=15);
            let fs = fluid_state_at(&view, 1.0, 1.62);
            assert!(
                fs.under_water(),
                "eye inside {name} must read as under water, got {fs:?}"
            );
        }

        let dry = [
            "minecraft:oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=false]",
            "minecraft:oak_slab[type=bottom,waterlogged=false]",
            "minecraft:short_grass",
            "minecraft:fern",
        ];
        for name in dry {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 0..=15);
            let fs = fluid_state_at(&view, 1.0, 1.62);
            assert!(
                !fs.under_water(),
                "eye inside {name} must NOT read as under water, got {fs:?}"
            );
        }
    }

    /// Lava is the same question with the other answer: the live view used to
    /// leave [`CollisionView::is_lava`] at its `false` default, so a live player
    /// could stand in lava and read as dry.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn lava_submerges_the_eye_and_is_not_water() {
        let atlas = vanilla_atlas();
        let id = state_id(&atlas, "minecraft:lava[level=0]");
        let view = live_column(Arc::clone(&atlas), id, 0..=15);
        let fs = fluid_state_at(&view, 1.0, 1.62);
        assert!(
            fs.under_lava(),
            "eye inside lava must read as under lava: {fs:?}"
        );
        assert!(!fs.under_water(), "lava must not read as water: {fs:?}");
    }

    /// The boundary the four consumers now share: **eye exactly at the water
    /// surface**. Vanilla's `isEyeInFluid` test is `eyeY <= fluidTop` — inclusive
    /// — so an eye resting exactly on the surface plane counts as submerged.
    /// Pinned here because fog, overlay, sounds and pose all flip on it.
    ///
    /// The column is water up to `y = 2` (top plane `y = 3.0`, the coarse
    /// full-cell height this adapter commits to). The eye height is chosen so the
    /// eye Y is *exactly* `3.0` with no float slop, and the two neighbours
    /// straddle it.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn eye_exactly_at_the_water_surface_counts_as_submerged() {
        let atlas = vanilla_atlas();
        let id = state_id(&atlas, "minecraft:water[level=0]");
        let view = live_column(Arc::clone(&atlas), id, 0..=2);

        // Eye exactly on the surface plane: 2.0 + 1.0 == 3.0, both exact in f32
        // and f64, so this is the true boundary and not a rounding artefact.
        let on = fluid_state_at(&view, 2.0, 1.0);
        assert!(
            on.under_water(),
            "an eye exactly at the water surface is submerged (vanilla's <=): {on:?}"
        );

        // A hair above the surface: dry eye, but the box is still in water, so
        // `in_water` stays true while `under_water` flips. That pair is what makes
        // this a boundary rather than an on/off.
        let above = fluid_state_at(&view, 2.0, 1.001);
        assert!(
            !above.under_water(),
            "an eye above the surface is not submerged: {above:?}"
        );
        assert!(
            above.in_water(),
            "the box is still in water even when the eye is out: {above:?}"
        );

        // A hair below: submerged.
        let below = fluid_state_at(&view, 2.0, 0.999);
        assert!(
            below.under_water(),
            "an eye below the surface is submerged: {below:?}"
        );
    }
}
