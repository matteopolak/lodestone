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
//! # …but "is this cell fluid" is not "can I break what is in it"
//!
//! Unifying the fluid answer immediately broke a *second* consumer that had been
//! borrowing it: the pick ray in `Sim::update_target` used `!is_water(cell)` as
//! shorthand for "this cell holds something breakable". Once `is_water` correctly
//! included kelp, seagrass and every waterlogged stair, that shorthand started
//! refusing all of them — **kelp could not be broken, because it could not be
//! targeted**. Vanilla asks a genuinely different question there (the *outline*
//! shape, with fluid shapes switched off), so the answer lives in its own
//! predicate, [`LiveCollision::is_pickable`], not in a negation of `is_water`. One
//! question one answer is right; the mistake was assuming there was one question.
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

    /// Whether the view ray can *target* this cell — the demo counterpart of
    /// [`LiveCollision::is_pickable`], which carries the full explanation.
    ///
    /// The demo palette has no waterlogging, no plants sharing a cell with water
    /// and no air variants, so here the question really does reduce to "not air and
    /// not the water block".
    #[must_use]
    pub fn is_pickable(&self, x: i32, y: i32, z: i32) -> bool {
        let b = self.block_at(x, y, z);
        b != id::AIR && demo_fluid(b).is_none()
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
    /// Resolved state ids of the three air blocks (see [`AIR_BLOCKS`]), for
    /// [`is_pickable`](Self::is_pickable). Small and fixed, so a linear scan beats
    /// a set.
    air_states: Vec<u32>,
}

/// The blocks whose **outline** shape is `Shapes.empty()` *and* whose cell holds
/// no fluid, i.e. the ones `Entity.pick` must walk straight through without them
/// being identifiable as "a fluid cell".
///
/// All three register as `AirBlock` (`Blocks.java:4273-4278`), whose `getShape`
/// returns `Shapes.empty()` (`AirBlock.java:30-32`), so none of them is targetable
/// in vanilla. This matters because **`minecraft:air` is not the only air**:
/// `WorldCarver` writes `Blocks.CAVE_AIR` (`WorldCarver.java:36`), as do lakes,
/// monster rooms and strongholds, and the end's void column is `void_air`. Each is
/// a *distinct block-state id*, so a pick predicate written as `state_id != 0`
/// targets the empty space one block in front of the player's face in any carved
/// cave, in preference to whatever real block is behind it.
const AIR_BLOCKS: [&str; 3] = ["minecraft:air", "minecraft:cave_air", "minecraft:void_air"];

impl LiveCollision {
    /// Build a view from a pre-fetched section snapshot and the dimension geometry.
    #[must_use]
    pub fn new(
        sections: HashMap<(i32, i32, usize), Arc<ChunkSection>>,
        min_y: i32,
        section_count: usize,
        atlas: Arc<BlockAtlas>,
    ) -> Self {
        // Three `state_id_of` lookups (a hash of a `(name, properties)` key each)
        // per snapshot. The snapshot itself already clones ~200 `Arc<ChunkSection>`
        // handles, so this is noise; resolving by *name* rather than hardcoding ids
        // is what keeps the list checkable against the jar.
        //
        // `0` is seeded unconditionally: it is `minecraft:air`, and it is also what
        // `block_at` returns for a cell outside the snapshot, so a missing name
        // index must never make unloaded space targetable.
        let mut air_states = vec![0u32];
        for name in AIR_BLOCKS {
            if let Some(id) = atlas.state_id_of(name)
                && !air_states.contains(&id)
            {
                air_states.push(id);
            }
        }
        Self {
            sections,
            min_y,
            section_count,
            atlas,
            air_states,
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

    /// Whether the view ray can **target** this cell — the client-side stand-in for
    /// vanilla's `clip(ClipContext.Block.OUTLINE, ClipContext.Fluid.NONE)`.
    ///
    /// # This is a different question from `is_water`, and conflating them is the bug
    ///
    /// Picking is *not* collision (a cross-plant has an empty collision shape and is
    /// still breakable), and it is *not* "the cell has no fluid" either. Vanilla
    /// walks the **outline** shape (`BlockStateBase.getShape`) with fluid shapes
    /// switched off (`Fluid.NONE`, `Entity.pick`, `Entity.java:2012-2017`). So the
    /// real question is *does the block in this cell have a non-empty outline*:
    ///
    /// * `LiquidBlock.getShape` → `Shapes.empty()` (`LiquidBlock.java:145-147`), so
    ///   open water and lava are never targeted;
    /// * `KelpBlock`'s is `Block.column(16, 0, 9)` (`KelpBlock.java:24`) and
    ///   `SeagrassBlock`'s is `Block.column(12, 0, 12)` (`SeagrassBlock.java:29`) —
    ///   **non-empty**, so kelp and seagrass are targeted and breakable, even though
    ///   both hardcode `getFluidState` → `Fluids.WATER`.
    ///
    /// The pick used to be `is_solid(cell) || (state != AIR && !is_water && !is_lava)`.
    /// That worked only for as long as `is_water` meant literally
    /// `minecraft:water`. Fixing `is_water` to mean *"this cell has a fluid"* — the
    /// correct answer for buoyancy, fog and the overlay, and the whole point of the
    /// one-classifier rule in this module's docs — silently made `!is_water` false
    /// for **kelp, seagrass, tall seagrass, bubble columns and every
    /// `waterlogged=true` stair/slab/fence**. All of them are non-occluding, so
    /// `is_solid` was false too: the ray passed straight through and `Sim::target`
    /// stayed `None`, which makes `drive_mining` abort before it ever sends a
    /// `START_DESTROY`. The dig was not slow or refused — the block was never
    /// *selected*. One classifier, but **two** questions.
    ///
    /// # What this actually tests, and where it is coarse
    ///
    /// There is no outline-shape table in this repo (`collision_shapes` is a
    /// collision-only oracle dump, and kelp's collision shape is empty too), so the
    /// nearest available proxy for "has an outline" is *has baked model geometry*.
    /// That coincidence is structural, not luck: fluids are the one thing vanilla
    /// does **not** draw through the block-model pipeline — their blockstate models
    /// are empty — which is exactly why `BlockModels::quads` is empty for a fluid
    /// state and non-empty for kelp (see [`lodestone_render::FluidCell`]).
    ///
    /// A cell is pickable when it is:
    /// 1. not one of the three air blocks ([`AIR_BLOCKS`]); **and**
    /// 2. either it has baked quads (every real block, plant, slab, stair, and any
    ///    waterlogged form of them), **or** it has no fluid — clause 2's second half
    ///    keeps the geometry-less-but-real blocks targetable (`barrier`, `light`,
    ///    `structure_void`, and anything whose model failed to bake), which is what
    ///    the predicate this replaces already did.
    ///
    /// [`is_solid`](Self::is_solid) implies this, so it is not tested separately: an
    /// occluding cube is `is_full_cube(quads) && layer == Solid`, which cannot hold
    /// with no quads.
    ///
    /// Still coarse, unchanged from before: a picked cell is treated as a full unit
    /// cube, so anything with a genuinely partial outline (a slab, a stair, kelp's
    /// own 9/16 height) over-selects at its edges. The real fix is an
    /// outline/interaction-shape oracle dump alongside the collision one.
    #[must_use]
    pub fn is_pickable(&self, x: i32, y: i32, z: i32) -> bool {
        let state = self.block_at(x, y, z);
        if self.air_states.contains(&state) {
            return false;
        }
        if let Some(models) = self.atlas.models()
            && !models.quads(state).is_empty()
        {
            return true;
        }
        vanilla_fluid(&self.atlas, state).is_none()
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

    /// **The kelp bug.** Every one of these `wet` states carries a water source, so
    /// the pick predicate that asked `!is_water(cell)` refused all of them: kelp
    /// could not be targeted, so it could not be broken, and neither could a
    /// waterlogged stair or slab. None of them is a full cube either, so `is_solid`
    /// did not save it. Vanilla targets all of them, because their **outline**
    /// shapes are non-empty (`KelpBlock.java:24`, `SeagrassBlock.java:29`) and
    /// `Entity.pick` runs with `ClipContext.Fluid.NONE`.
    ///
    /// The `dry` list is not decoration, it is the control: open water and lava must
    /// stay un-targetable (else you would break the ocean instead of the sand under
    /// it) and so must all three air blocks — including `cave_air`, which is what a
    /// carved cave is full of and which any `state_id != 0` test wrongly targets.
    /// Without those five failing, "kelp is pickable" would be satisfied by a
    /// predicate that just returns `true`.
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn submerged_plants_and_waterlogged_blocks_are_pickable_but_open_fluid_is_not() {
        let atlas = vanilla_atlas();

        let pickable = [
            // The reported symptom.
            "minecraft:kelp_plant",
            "minecraft:kelp[age=0]",
            "minecraft:seagrass",
            "minecraft:tall_seagrass[half=lower]",
            "minecraft:tall_seagrass[half=upper]",
            // The same defect on blocks that carry water via `waterlogged`.
            "minecraft:oak_stairs[facing=north,half=bottom,shape=straight,waterlogged=true]",
            "minecraft:oak_slab[type=bottom,waterlogged=true]",
            "minecraft:oak_fence[east=false,north=false,south=false,waterlogged=true,west=false]",
            // Already worked, kept so a regression the other way is visible: a dry
            // shapeless plant and a plain full cube.
            "minecraft:short_grass",
            "minecraft:stone",
            "minecraft:oak_slab[type=bottom,waterlogged=false]",
        ];
        for name in pickable {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 0..=15);
            assert!(
                view.is_pickable(0, 1, 0),
                "{name} must be targetable by the view ray"
            );
        }

        let not_pickable = [
            "minecraft:water[level=0]",
            "minecraft:water[level=3]",
            "minecraft:lava[level=0]",
            "minecraft:air",
            "minecraft:cave_air",
            "minecraft:void_air",
        ];
        for name in not_pickable {
            let id = state_id(&atlas, name);
            let view = live_column(Arc::clone(&atlas), id, 0..=15);
            assert!(
                !view.is_pickable(0, 1, 0),
                "{name} must NOT be targetable by the view ray"
            );
        }
    }

    /// The pick predicate is only useful if the *ray* honours it, and the ray is
    /// what `Sim::update_target` runs. Fire the real
    /// [`crate::raycast::raycast`] through a kelp cell with a stone block behind it:
    /// the near cell must win. Before the fix the ray skipped the kelp and reported
    /// the stone — a targeting bug that reads on screen as "kelp cannot be broken".
    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn the_view_ray_stops_at_kelp_rather_than_the_block_behind_it() {
        let atlas = vanilla_atlas();
        let kelp = state_id(&atlas, "minecraft:kelp_plant");
        let stone = state_id(&atlas, "minecraft:stone");

        // One section: kelp at y = 4, stone at y = 2, air between. Looking straight
        // down from y = 6.5 the ray meets kelp first.
        let mut section = ChunkSection::new(
            PaletteKind::block_states_with_direct_bits(20),
            PaletteKind::biomes(),
            0,
            0,
        );
        for x in 0..16 {
            for z in 0..16 {
                section.set_block(x, 4, z, kelp);
                section.set_block(x, 2, z, stone);
            }
        }
        let mut sections = HashMap::new();
        sections.insert((0, 0, 0), Arc::new(section));
        let view = LiveCollision::new(sections, 0, 1, Arc::clone(&atlas));

        let hit = crate::raycast::raycast([0.5, 6.5, 0.5], [0.0, -1.0, 0.0], 4.0, |x, y, z| {
            view.is_pickable(x, y, z)
        })
        .expect("the ray must hit something within 4 blocks");
        assert_eq!(
            hit.block,
            [0, 4, 0],
            "the ray must stop at the kelp, not tunnel through to the stone"
        );
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
