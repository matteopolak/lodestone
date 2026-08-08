//! **Coded** structure pieces — the ones whose blocks are Java statements rather
//! than an `.nbt` template (issue #514's S5).
//!
//! # What it is
//!
//! A port of `StructurePiece`'s block-writing helpers (`placeBlock`,
//! `generateBox`, `fillColumnDown`, the `getWorldX/Y/Z` orientation transform) plus
//! `ScatteredFeaturePiece`'s two ground-height rules, and on top of them the piece
//! generators for `swamp_hut` and `desert_pyramid`.
//!
//! # How it works
//!
//! Vanilla's coded generators write **into the world** as they walk, from whichever
//! chunk's `postProcess` reaches them first, reading heights and existing blocks at
//! arbitrary positions and freely crossing chunk borders. Our chunks are generated
//! independently and memoised, so that is unavailable — the same wall S2 hit for
//! template piece Y, one step further along.
//!
//! So a [`Builder`] accumulates the piece's whole block list **eagerly at start
//! time**, against [`StartContext`], and `structure_place_stage` clips it per chunk
//! ([`StructurePiece::blocks`]). `chunkBB` disappears from every signature: a write
//! that vanilla would have skipped because it was outside the decorating chunk is
//! recorded here and clipped later, which is the same set of blocks in the same
//! last-write-wins order.
//!
//! ```text
//! Builder::new(west, north, orientation, w, h, d)   <- makeBoundingBox + setOrientation
//! builder.lowest_ground_height(ctx, offset)?        <- moves the box down onto the terrain
//! builder.generate_box(...) / place(...) / fill_column_down(ctx, ...)
//! builder.finish(piece_id)                          -> StructurePiece
//! ```
//!
//! # How to change it
//!
//! * **`place` is last-write-wins and every generator depends on it.** A pyramid
//!   carves its corridors by writing `air` over sandstone it placed two statements
//!   earlier. Deduplicating or reordering the list breaks it.
//! * **Local coordinates are not world coordinates**, and the mapping depends on
//!   the piece's orientation ([`Builder::world_x`]). A NORTH piece's local Z counts
//!   *down* from the box maximum; a WEST piece swaps the axes. Alongside that,
//!   `setOrientation` gives SOUTH and WEST a `LEFT_RIGHT` **mirror**, which is what
//!   makes `BlockState::mirror`'s stair `shape` handling load-bearing rather than
//!   the inert ledger row it was for templates.
//! * **The two ground rules are different functions and the difference matters.**
//!   `updateHeightPositionToLowestGroundHeight` scans the whole piece box and is
//!   chunk-independent in vanilla too; `updateAverageGroundHeight` averages over
//!   *the intersection of the box with the decorating chunk*, so vanilla's own
//!   answer depends on which chunk got there first. [`Builder::average_ground_height`]
//!   deliberately averages over the **whole box** — see its doc.
//!
//! # Dependencies
//!
//! [`StartContext`] for terrain heights and column contents, and
//! [`super::template::BlockState`] for the mirror/rotate transform `placeBlock`
//! applies.

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;

use super::template::{BlockState, Mirror, Rotation};
use super::{
    BoundingBox, CodedBlock, HeightmapKind, StartContext, StructurePiece, free_height,
};

/// A horizontal `Direction`, in `Direction.Plane.HORIZONTAL` order — which is the
/// order `getRandomHorizontalDirection`'s single `nextInt(4)` indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Facing {
    /// `NORTH`, 2D data value 2.
    North,
    /// `EAST`, 2D data value 3.
    East,
    /// `SOUTH`, 2D data value 0.
    South,
    /// `WEST`, 2D data value 1.
    West,
}

impl Facing {
    /// `getRandomHorizontalDirection(random)` — `Util.getRandom(HORIZONTAL.faces,
    /// random)`, exactly one `nextInt(4)`, over `{NORTH, EAST, SOUTH, WEST}`.
    pub fn random<R: RandomSource>(random: &mut R) -> Self {
        match random.next_int_bounded(4) {
            1 => Self::East,
            2 => Self::South,
            3 => Self::West,
            _ => Self::North,
        }
    }

    /// `get2DDataValue()` — the value persisted as a piece's `O`.
    #[must_use]
    pub fn data_2d(self) -> i32 {
        match self {
            Self::South => 0,
            Self::West => 1,
            Self::North => 2,
            Self::East => 3,
        }
    }

    /// True when this direction's axis is Z, i.e. `makeBoundingBox` keeps
    /// `(width, depth)` in `(x, z)` rather than swapping them.
    fn is_z_axis(self) -> bool {
        matches!(self, Self::North | Self::South)
    }

    /// `setOrientation`'s `(mirror, rotation)` pair.
    ///
    /// Not derivable from the facing by any obvious rule — SOUTH mirrors but does
    /// not rotate, WEST does both, EAST only rotates, NORTH neither — so it is a
    /// table, transcribed.
    fn transform(self) -> (Mirror, Rotation) {
        match self {
            Self::South => (Mirror::LeftRight, Rotation::None),
            Self::West => (Mirror::LeftRight, Rotation::Cw90),
            Self::East => (Mirror::None, Rotation::Cw90),
            Self::North => (Mirror::None, Rotation::None),
        }
    }
}

/// One coded piece under construction.
#[derive(Debug)]
pub struct Builder {
    box_: BoundingBox,
    orientation: Facing,
    mirror: Mirror,
    rotation: Rotation,
    height_position: Option<i32>,
    blocks: Vec<CodedBlock>,
    loot: Vec<super::CodedLoot>,
}

impl Builder {
    /// `ScatteredFeaturePiece`'s constructor: `makeBoundingBox(west, floor, north,
    /// direction, width, height, depth)` plus `setOrientation(direction)`.
    ///
    /// `floor` is the literal 64 every scattered piece starts at, before
    /// [`Self::lowest_ground_height`] or [`Self::average_ground_height`] moves it.
    #[must_use]
    pub fn new(
        west: i32,
        floor: i32,
        north: i32,
        orientation: Facing,
        width: i32,
        height: i32,
        depth: i32,
    ) -> Self {
        // The axis swap is the whole of `makeBoundingBox`: an X-axis orientation
        // lays the piece out `depth` blocks along x and `width` along z.
        let (dx, dz) = if orientation.is_z_axis() {
            (width, depth)
        } else {
            (depth, width)
        };
        let (mirror, rotation) = orientation.transform();
        Self {
            box_: BoundingBox {
                min: [west, floor, north],
                max: [west + dx - 1, floor + height - 1, north + dz - 1],
            },
            orientation,
            mirror,
            rotation,
            height_position: None,
            blocks: Vec::new(),
            loot: Vec::new(),
        }
    }

    /// The piece's current bounding box.
    #[must_use]
    pub fn bounding_box(&self) -> BoundingBox {
        self.box_
    }

    /// `updateHeightPositionToLowestGroundHeight(level, offset)`.
    ///
    /// Scans **every** column of the box for the lowest ground height and moves the
    /// box down onto it. Chunk-independent in vanilla as well as here — it is the
    /// *average* variant that is not.
    ///
    /// Returns false where vanilla returns false (an empty box), which aborts the
    /// whole `postProcess` and therefore the whole piece.
    pub fn lowest_ground_height(&mut self, ctx: &dyn StartContext, offset: i32) -> bool {
        if self.height_position.is_some() {
            return true;
        }
        let mut lowest = i32::MAX;
        let mut found = false;
        for z in self.box_.min[2]..=self.box_.max[2] {
            for x in self.box_.min[0]..=self.box_.max[0] {
                lowest = lowest.min(free_height(ctx, x, z, HeightmapKind::OceanFloorWg));
                found = true;
            }
        }
        if !found {
            return false;
        }
        self.set_height_position(lowest, offset);
        true
    }

    /// `updateAverageGroundHeight(level, chunkBB, offset)`, made
    /// **order-independent**.
    ///
    /// # The deviation, and why it is the right one
    ///
    /// Vanilla averages the heightmap over *the intersection of the piece box with
    /// the decorating chunk*, so its answer depends on which of the chunks a hut
    /// spans generated first — a real order dependence in vanilla, not an artefact
    /// of our pipeline. There is therefore no "vanilla answer" to reproduce: there
    /// are up to four of them for one hut, and vanilla resolves the ambiguity by
    /// memoising whichever it computed first into `HPos`.
    ///
    /// This averages over the **whole box**, which is:
    ///
    /// * the same value vanilla computes whenever the piece lies inside one chunk
    ///   (a 7×9 hut placed from the chunk's min corner spans at most two chunks, so
    ///   this is a real fraction of cases, not a degenerate one);
    /// * the arithmetic mean of vanilla's per-chunk answers weighted by area, so it
    ///   never sits outside their range;
    /// * a pure function of the seed and the chunk, which is the property the whole
    ///   engine depends on. A hut whose Y depended on visit order would shear along
    ///   a chunk border, and the two halves would be at different heights on
    ///   *reload* as well, since only one `HPos` is persisted.
    ///
    /// Recorded on the ledger as `coded:average_ground_height`.
    pub fn average_ground_height(&mut self, ctx: &dyn StartContext, offset: i32) -> bool {
        if self.height_position.is_some() {
            return true;
        }
        let mut total: i64 = 0;
        let mut count: i64 = 0;
        for z in self.box_.min[2]..=self.box_.max[2] {
            for x in self.box_.min[0]..=self.box_.max[0] {
                // `MOTION_BLOCKING_NO_LEAVES` against a generated chunk; the `_WG`
                // analogue that exists before terrain is `OCEAN_FLOOR_WG`.
                total += i64::from(free_height(ctx, x, z, HeightmapKind::OceanFloorWg));
                count += 1;
            }
        }
        if count == 0 {
            return false;
        }
        let average = i32::try_from(total / count).unwrap_or(0);
        self.set_height_position(average, offset);
        true
    }

    fn set_height_position(&mut self, height: i32, offset: i32) {
        let shift = height - self.box_.min[1] + offset;
        self.box_.min[1] += shift;
        self.box_.max[1] += shift;
        self.height_position = Some(height);
    }

    /// `getWorldX(x, z)`.
    #[must_use]
    pub fn world_x(&self, x: i32, z: i32) -> i32 {
        match self.orientation {
            Facing::North | Facing::South => self.box_.min[0] + x,
            Facing::West => self.box_.max[0] - z,
            Facing::East => self.box_.min[0] + z,
        }
    }

    /// `getWorldY(y)`.
    #[must_use]
    pub fn world_y(&self, y: i32) -> i32 {
        y + self.box_.min[1]
    }

    /// `getWorldZ(x, z)`.
    #[must_use]
    pub fn world_z(&self, x: i32, z: i32) -> i32 {
        match self.orientation {
            Facing::North => self.box_.max[2] - z,
            Facing::South => self.box_.min[2] + z,
            Facing::West | Facing::East => self.box_.min[2] + x,
        }
    }

    /// `getWorldPos(x, y, z)`.
    #[must_use]
    pub fn world_pos(&self, x: i32, y: i32, z: i32) -> [i32; 3] {
        [self.world_x(x, z), self.world_y(y), self.world_z(x, z)]
    }

    /// `placeBlock(level, state, x, y, z, chunkBB)` — mirror, then rotate, then
    /// record.
    ///
    /// `canBeReplaced` is `true` for every piece here (only mineshaft overrides it)
    /// and the `chunkBB` test becomes the clip at write time.
    pub fn place(&mut self, state: &BlockState, x: i32, y: i32, z: i32) {
        let transformed = state.mirror(self.mirror).rotate(self.rotation);
        let pos = self.world_pos(x, y, z);
        self.blocks.push(CodedBlock {
            pos,
            state: transformed.canonical(),
        });
    }

    /// `generateBox(..., edge, fill, skipAir = false)`.
    ///
    /// A block is `edge` when it is on any face of the box and `fill` otherwise —
    /// so a 1-thick box is entirely `edge`, which is how the same call spells both
    /// "hollow shell" and "solid slab".
    #[allow(clippy::too_many_arguments)]
    pub fn generate_box(
        &mut self,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        edge: &BlockState,
        fill: &BlockState,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    let interior = y != y0 && y != y1 && x != x0 && x != x1 && z != z0 && z != z1;
                    let state = if interior { fill } else { edge };
                    self.place(state, x, y, z);
                }
            }
        }
    }

    /// `generateAirBox(level, chunkBB, x0, y0, z0, x1, y1, z1)`.
    ///
    /// Not `generate_box` with air for both arguments: it is a distinct vanilla
    /// method and spelling it out keeps the transcription line-for-line, which is
    /// the property that makes a 300-statement piece reviewable at all.
    #[allow(clippy::too_many_arguments)]
    pub fn generate_air_box(&mut self, x0: i32, y0: i32, z0: i32, x1: i32, y1: i32, z1: i32) {
        let air = BlockState::of("minecraft:air");
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    self.place(&air, x, y, z);
                }
            }
        }
    }

    /// `generateBox(..., skipAir = false, random, selector)` — the
    /// [`StructurePiece.BlockSelector`] overload.
    ///
    /// **The draw count is the specification.** `selector.next` is called for
    /// *every* position in the box, before `placeBlock`, so a box of `n` positions
    /// consumes exactly `n` selector draws whether or not each write lands inside
    /// the decorating chunk. Skipping a position — for instance to "optimise" a box
    /// that is entirely outside the served chunk — desynchronises the stream for
    /// everything after it.
    ///
    /// `is_edge` is vanilla's inline
    /// `y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1`, which is
    /// the *negation* of [`Self::generate_box`]'s `interior` test — not
    /// `isInterior`, the unrelated heightmap probe on the same class.
    #[allow(clippy::too_many_arguments)]
    pub fn generate_box_selected<R: RandomSource>(
        &mut self,
        x0: i32,
        y0: i32,
        z0: i32,
        x1: i32,
        y1: i32,
        z1: i32,
        random: &mut R,
        selector: &mut dyn FnMut(&mut R, i32, i32, i32, bool) -> BlockState,
    ) {
        for y in y0..=y1 {
            for x in x0..=x1 {
                for z in z0..=z1 {
                    let is_edge = y == y0 || y == y1 || x == x0 || x == x1 || z == z0 || z == z1;
                    let state = selector(random, x, y, z, is_edge);
                    self.place(&state, x, y, z);
                }
            }
        }
    }

    /// `createChest(level, chunkBB, random, x, y, z, lootTable)`.
    ///
    /// Three faithfulness notes, each of which would be invisible if it were wrong:
    ///
    /// * **`createChest` calls `level.setBlock` directly, not `placeBlock`**, so the
    ///   piece's mirror/rotation is *not* applied to the chest — unlike
    ///   [`Self::create_dispenser`], which does go through `placeBlock`. The state
    ///   is pushed raw.
    /// * `random.nextLong()` is drawn **whenever the chest lands inside the
    ///   decorating chunk**, and vanilla's `placedMainChest` flag makes that happen
    ///   exactly once per piece across all the chunk passes. One draw here, in
    ///   source order, is the same total.
    /// * vanilla's facing comes from `StructurePiece.reorient`, which reads the
    ///   render-solidity of the four horizontal neighbours *of the world as written
    ///   so far*. There is no block-state read on [`StartContext`] and no solidity
    ///   table in this crate, so the default `facing=north` is kept and the
    ///   deviation is on the ledger as `coded:chest_reorient`.
    pub fn create_chest<R: RandomSource>(
        &mut self,
        random: &mut R,
        x: i32,
        y: i32,
        z: i32,
        table: &str,
    ) {
        let pos = self.world_pos(x, y, z);
        self.blocks.push(CodedBlock {
            pos,
            state: "minecraft:chest[facing=north,type=single,waterlogged=false]".to_string(),
        });
        let seed = random.next_long();
        self.loot.push(super::CodedLoot {
            pos,
            table: table.to_string(),
            seed,
        });
    }

    /// `createDispenser(level, chunkBB, random, x, y, z, facing, lootTable)`.
    ///
    /// Unlike [`Self::create_chest`] this one *does* route through `placeBlock`, so
    /// the dispenser's `facing` is mirrored and rotated with the piece.
    pub fn create_dispenser<R: RandomSource>(
        &mut self,
        random: &mut R,
        x: i32,
        y: i32,
        z: i32,
        facing: &str,
        table: &str,
    ) {
        self.place(
            &BlockState::parse(&format!("minecraft:dispenser[facing={facing},triggered=false]")),
            x,
            y,
            z,
        );
        let seed = random.next_long();
        self.loot.push(super::CodedLoot {
            pos: self.world_pos(x, y, z),
            table: table.to_string(),
            seed,
        });
    }

    /// `fillColumnDown(level, state, x, startY, z, chunkBB)` — write downward from
    /// `startY` while the column is replaceable.
    ///
    /// The one helper that reads the world, through
    /// [`StartContext::is_replaceable_at`]. The `pos.getY() > level.getMinY() + 1`
    /// bound is vanilla's and is what stops a stilt over a void column running to
    /// the bottom of the world.
    pub fn fill_column_down(
        &mut self,
        ctx: &dyn StartContext,
        state: &BlockState,
        x: i32,
        start_y: i32,
        z: i32,
    ) {
        let mut pos = self.world_pos(x, start_y, z);
        let floor = ctx.min_y() + 1;
        while pos[1] > floor && ctx.is_replaceable_at(pos[0], pos[1], pos[2]) {
            let transformed = state.mirror(self.mirror).rotate(self.rotation);
            self.blocks.push(CodedBlock {
                pos,
                state: transformed.canonical(),
            });
            pos[1] -= 1;
        }
    }

    /// How many blocks have been recorded — for a gate that wants a count without
    /// consuming the builder.
    #[must_use]
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// True when nothing has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Turns the builder into the piece the placement stage consumes.
    #[must_use]
    pub fn finish(self, id: &str) -> StructurePiece {
        StructurePiece {
            id: id.to_string(),
            bounding_box: self.box_,
            orientation: Some(self.orientation.data_2d()),
            gen_depth: 0,
            template: None,
            placement: None,
            extra_placements: Vec::new(),
            blocks: Some(Arc::new(self.blocks)),
            loot: self.loot,
            // `Beardifier.java:75`'s `else` branch: a non-pool piece is a rigid box
            // with `groundLevelDelta` 0 and no junctions. Inert for both structures
            // here, whose `terrain_adaptation` is `none`.
            beard: None,
        }
    }
}

/// A shorthand for the many `Blocks.X.defaultBlockState()`s below.
fn s(spec: &str) -> BlockState {
    BlockState::parse(spec)
}

/// `SwampHutStructure.generatePieces` → `SwampHutPiece`.
///
/// The whole piece, minus the witch and the cat: entity spawning at worldgen time
/// has no driver in this engine, and the gap is on the ledger
/// (`coded:worldgen_entities`) rather than silently absent.
#[must_use]
pub fn swamp_hut_pieces<R: RandomSource>(
    cx: i32,
    cz: i32,
    ctx: &dyn StartContext,
    random: &mut R,
) -> Vec<StructurePiece> {
    // The one RNG draw the whole structure makes.
    let orientation = Facing::random(random);
    let mut b = Builder::new(cx * 16, 64, cz * 16, orientation, 7, 7, 9);
    if !b.average_ground_height(ctx, 0) {
        return Vec::new();
    }
    let planks = s("minecraft:spruce_planks");
    let log = s("minecraft:oak_log[axis=y]");
    let air = s("minecraft:air");
    let fence = s("minecraft:oak_fence[east=false,north=false,south=false,waterlogged=false,west=false]");
    b.generate_box(1, 1, 1, 5, 1, 7, &planks, &planks);
    b.generate_box(1, 4, 2, 5, 4, 7, &planks, &planks);
    b.generate_box(2, 1, 0, 4, 1, 0, &planks, &planks);
    b.generate_box(2, 2, 2, 3, 3, 2, &planks, &planks);
    b.generate_box(1, 2, 3, 1, 3, 6, &planks, &planks);
    b.generate_box(5, 2, 3, 5, 3, 6, &planks, &planks);
    b.generate_box(2, 2, 7, 4, 3, 7, &planks, &planks);
    b.generate_box(1, 0, 2, 1, 3, 2, &log, &log);
    b.generate_box(5, 0, 2, 5, 3, 2, &log, &log);
    b.generate_box(1, 0, 7, 1, 3, 7, &log, &log);
    b.generate_box(5, 0, 7, 5, 3, 7, &log, &log);
    b.place(&fence, 2, 3, 2);
    b.place(&fence, 3, 3, 7);
    b.place(&air, 1, 3, 4);
    b.place(&air, 5, 3, 4);
    b.place(&air, 5, 3, 5);
    b.place(&s("minecraft:potted_red_mushroom"), 1, 3, 5);
    b.place(&s("minecraft:crafting_table"), 3, 2, 6);
    b.place(&s("minecraft:cauldron"), 4, 2, 6);
    b.place(&fence, 1, 2, 1);
    b.place(&fence, 5, 2, 1);
    let stairs = |facing: &str, shape: &str| {
        s(&format!(
            "minecraft:spruce_stairs[facing={facing},half=bottom,shape={shape},waterlogged=false]"
        ))
    };
    let north = stairs("north", "straight");
    let east = stairs("east", "straight");
    let west = stairs("west", "straight");
    let south = stairs("south", "straight");
    b.generate_box(0, 4, 1, 6, 4, 1, &north, &north);
    b.generate_box(0, 4, 2, 0, 4, 7, &east, &east);
    b.generate_box(6, 4, 2, 6, 4, 7, &west, &west);
    b.generate_box(0, 4, 8, 6, 4, 8, &south, &south);
    // The four roof corners, whose `shape` is set explicitly — and which is why
    // the stair half of `BlockState::mirror` had to land with this.
    b.place(&stairs("north", "outer_right"), 0, 4, 1);
    b.place(&stairs("north", "outer_left"), 6, 4, 1);
    b.place(&stairs("south", "outer_left"), 0, 4, 8);
    b.place(&stairs("south", "outer_right"), 6, 4, 8);
    // The four stilts.
    let mut z = 2;
    while z <= 7 {
        let mut x = 1;
        while x <= 5 {
            b.fill_column_down(ctx, &log, x, -1, z);
            x += 4;
        }
        z += 5;
    }
    vec![b.finish("minecraft:tesw")]
}

/// `DesertPyramidStructure` → `DesertPyramidPiece`, including the cellar and the
/// `afterPlace` suspicious-sand pass.
///
/// # Two deviations, both forced and both the same shape
///
/// * Vanilla's cellar `variant` boolean and its collapsed-roof `nextFloat() < 0.33`
///   come from `level.getRandom()` — the *decorating region's* random, so vanilla's
///   own answer depends on which chunk placed the piece. Both are position-seeded
///   here, exactly as [`super::processor::Processor::BlockRot`] is, so two chunks
///   placing two halves of one pyramid agree.
/// * The chests (`createChest` ×4) need block entities and loot tables; the
///   suspicious sand's own loot table likewise. The **blocks** are placed and the
///   contents are on the ledger.
#[must_use]
pub fn desert_pyramid_pieces<R: RandomSource>(
    cx: i32,
    cz: i32,
    ctx: &dyn StartContext,
    random: &mut R,
) -> Vec<StructurePiece> {
    let orientation = Facing::random(random);
    // `updateHeightPositionToLowestGroundHeight(level, -random.nextInt(3))` — the
    // draw happens at `postProcess` time in vanilla, from the *decorating* random,
    // and comes out of the structure's own per-chunk stream here for the reason S2
    // recorded for the beached shipwreck's `nextInt(3)`.
    let sink = -random.next_int_bounded(3);
    let mut b = Builder::new(cx * 16, 64, cz * 16, orientation, PYRAMID_WIDTH, 15, PYRAMID_DEPTH);
    if !b.lowest_ground_height(ctx, sink) {
        return Vec::new();
    }
    let sandstone = s("minecraft:sandstone");
    let air = s("minecraft:air");
    let cut = s("minecraft:cut_sandstone");
    let chiseled = s("minecraft:chiseled_sandstone");
    let orange = s("minecraft:orange_terracotta");
    let blue = s("minecraft:blue_terracotta");
    let sand = s("minecraft:sand");
    let stairs = |facing: &str| {
        s(&format!(
            "minecraft:sandstone_stairs[facing={facing},half=bottom,shape=straight,waterlogged=false]"
        ))
    };
    let w = PYRAMID_WIDTH;
    let d = PYRAMID_DEPTH;

    b.generate_box(0, -4, 0, w - 1, 0, d - 1, &sandstone, &sandstone);
    for pos in 1..=9 {
        b.generate_box(pos, pos, pos, w - 1 - pos, pos, d - 1 - pos, &sandstone, &sandstone);
        b.generate_box(
            pos + 1,
            pos,
            pos + 1,
            w - 2 - pos,
            pos,
            d - 2 - pos,
            &air,
            &air,
        );
    }
    for x in 0..w {
        for z in 0..d {
            b.fill_column_down(ctx, &sandstone, x, -5, z);
        }
    }
    let north = stairs("north");
    let south = stairs("south");
    let east = stairs("east");
    let west = stairs("west");
    b.generate_box(0, 0, 0, 4, 9, 4, &sandstone, &air);
    b.generate_box(1, 10, 1, 3, 10, 3, &sandstone, &sandstone);
    b.place(&north, 2, 10, 0);
    b.place(&south, 2, 10, 4);
    b.place(&east, 0, 10, 2);
    b.place(&west, 4, 10, 2);
    b.generate_box(w - 5, 0, 0, w - 1, 9, 4, &sandstone, &air);
    b.generate_box(w - 4, 10, 1, w - 2, 10, 3, &sandstone, &sandstone);
    b.place(&north, w - 3, 10, 0);
    b.place(&south, w - 3, 10, 4);
    b.place(&east, w - 5, 10, 2);
    b.place(&west, w - 1, 10, 2);
    b.generate_box(8, 0, 0, 12, 4, 4, &sandstone, &air);
    b.generate_box(9, 1, 0, 11, 3, 4, &air, &air);
    for (x, y) in [(9, 1), (9, 2), (9, 3), (10, 3), (11, 3), (11, 2), (11, 1)] {
        b.place(&cut, x, y, 1);
    }
    b.generate_box(4, 1, 1, 8, 3, 3, &sandstone, &air);
    b.generate_box(4, 1, 2, 8, 2, 2, &air, &air);
    b.generate_box(12, 1, 1, 16, 3, 3, &sandstone, &air);
    b.generate_box(12, 1, 2, 16, 2, 2, &air, &air);
    b.generate_box(5, 4, 5, w - 6, 4, d - 6, &sandstone, &sandstone);
    b.generate_box(9, 4, 9, 11, 4, 11, &air, &air);
    b.generate_box(8, 1, 8, 8, 3, 8, &cut, &cut);
    b.generate_box(12, 1, 8, 12, 3, 8, &cut, &cut);
    b.generate_box(8, 1, 12, 8, 3, 12, &cut, &cut);
    b.generate_box(12, 1, 12, 12, 3, 12, &cut, &cut);
    b.generate_box(1, 1, 5, 4, 4, 11, &sandstone, &sandstone);
    b.generate_box(w - 5, 1, 5, w - 2, 4, 11, &sandstone, &sandstone);
    b.generate_box(6, 7, 9, 6, 7, 11, &sandstone, &sandstone);
    b.generate_box(w - 7, 7, 9, w - 7, 7, 11, &sandstone, &sandstone);
    b.generate_box(5, 5, 9, 5, 7, 11, &cut, &cut);
    b.generate_box(w - 6, 5, 9, w - 6, 7, 11, &cut, &cut);
    b.place(&air, 5, 5, 10);
    b.place(&air, 5, 6, 10);
    b.place(&air, 6, 6, 10);
    b.place(&air, w - 6, 5, 10);
    b.place(&air, w - 6, 6, 10);
    b.place(&air, w - 7, 6, 10);
    b.generate_box(2, 4, 4, 2, 6, 4, &air, &air);
    b.generate_box(w - 3, 4, 4, w - 3, 6, 4, &air, &air);
    b.place(&north, 2, 4, 5);
    b.place(&north, 2, 3, 4);
    b.place(&north, w - 3, 4, 5);
    b.place(&north, w - 3, 3, 4);
    b.generate_box(1, 1, 3, 2, 2, 3, &sandstone, &sandstone);
    b.generate_box(w - 3, 1, 3, w - 2, 2, 3, &sandstone, &sandstone);
    b.place(&sandstone, 1, 1, 2);
    b.place(&sandstone, w - 2, 1, 2);
    let slab = s("minecraft:sandstone_slab[type=bottom,waterlogged=false]");
    b.place(&slab, 1, 2, 2);
    b.place(&slab, w - 2, 2, 2);
    b.place(&west, 2, 1, 2);
    b.place(&east, w - 3, 1, 2);
    b.generate_box(4, 3, 5, 4, 3, 17, &sandstone, &sandstone);
    b.generate_box(w - 5, 3, 5, w - 5, 3, 17, &sandstone, &sandstone);
    b.generate_box(3, 1, 5, 4, 2, 16, &air, &air);
    b.generate_box(w - 6, 1, 5, w - 5, 2, 16, &air, &air);
    let mut z = 5;
    while z <= 17 {
        b.place(&cut, 4, 1, z);
        b.place(&chiseled, 4, 2, z);
        b.place(&cut, w - 5, 1, z);
        b.place(&chiseled, w - 5, 2, z);
        z += 2;
    }
    for (x, z) in [
        (10, 7),
        (10, 8),
        (9, 9),
        (11, 9),
        (8, 10),
        (12, 10),
        (7, 10),
        (13, 10),
        (9, 11),
        (11, 11),
        (10, 12),
        (10, 13),
    ] {
        b.place(&orange, x, 0, z);
    }
    b.place(&blue, 10, 0, 10);
    // The two side walls, `for (x = 0; x <= width - 1; x += width - 1)`.
    for x in [0, w - 1] {
        b.place(&cut, x, 2, 1);
        b.place(&orange, x, 2, 2);
        b.place(&cut, x, 2, 3);
        b.place(&cut, x, 3, 1);
        b.place(&orange, x, 3, 2);
        b.place(&cut, x, 3, 3);
        b.place(&orange, x, 4, 1);
        b.place(&chiseled, x, 4, 2);
        b.place(&orange, x, 4, 3);
        b.place(&cut, x, 5, 1);
        b.place(&orange, x, 5, 2);
        b.place(&cut, x, 5, 3);
        b.place(&orange, x, 6, 1);
        b.place(&chiseled, x, 6, 2);
        b.place(&orange, x, 6, 3);
        b.place(&orange, x, 7, 1);
        b.place(&orange, x, 7, 2);
        b.place(&orange, x, 7, 3);
        b.place(&cut, x, 8, 1);
        b.place(&cut, x, 8, 2);
        b.place(&cut, x, 8, 3);
    }
    // `for (x = 2; x <= width - 3; x += width - 3 - 2)`, i.e. x = 2 and x = 18.
    for x in [2, w - 3] {
        b.place(&cut, x - 1, 2, 0);
        b.place(&orange, x, 2, 0);
        b.place(&cut, x + 1, 2, 0);
        b.place(&cut, x - 1, 3, 0);
        b.place(&orange, x, 3, 0);
        b.place(&cut, x + 1, 3, 0);
        b.place(&orange, x - 1, 4, 0);
        b.place(&chiseled, x, 4, 0);
        b.place(&orange, x + 1, 4, 0);
        b.place(&cut, x - 1, 5, 0);
        b.place(&orange, x, 5, 0);
        b.place(&cut, x + 1, 5, 0);
        b.place(&orange, x - 1, 6, 0);
        b.place(&chiseled, x, 6, 0);
        b.place(&orange, x + 1, 6, 0);
        b.place(&orange, x - 1, 7, 0);
        b.place(&orange, x, 7, 0);
        b.place(&orange, x + 1, 7, 0);
        b.place(&cut, x - 1, 8, 0);
        b.place(&cut, x, 8, 0);
        b.place(&cut, x + 1, 8, 0);
    }
    b.generate_box(8, 4, 0, 12, 6, 0, &cut, &cut);
    b.place(&air, 8, 6, 0);
    b.place(&air, 12, 6, 0);
    b.place(&orange, 9, 5, 0);
    b.place(&chiseled, 10, 5, 0);
    b.place(&orange, 11, 5, 0);
    // The TNT trap room, below the pyramid.
    b.generate_box(8, -14, 8, 12, -11, 12, &cut, &cut);
    b.generate_box(8, -10, 8, 12, -10, 12, &chiseled, &chiseled);
    b.generate_box(8, -9, 8, 12, -9, 12, &cut, &cut);
    b.generate_box(8, -8, 8, 12, -1, 12, &sandstone, &sandstone);
    b.generate_box(9, -11, 9, 11, -1, 11, &air, &air);
    b.place(&s("minecraft:stone_pressure_plate[powered=false]"), 10, -11, 10);
    b.generate_box(9, -13, 9, 11, -13, 11, &s("minecraft:tnt[unstable=false]"), &air);
    for (dx, dz) in [(-2, 0), (2, 0), (0, -2), (0, 2)] {
        // The four alcoves, spelled out per axis in vanilla; each is two air blocks
        // and a chiseled/cut pair one further out.
        let (ax, az) = (10 + dx, 10 + dz);
        b.place(&air, ax, -11, az);
        b.place(&air, ax, -10, az);
        let (ox, oz) = (10 + dx / 2 * 3, 10 + dz / 2 * 3);
        b.place(&chiseled, ox, -10, oz);
        b.place(&cut, ox, -11, oz);
    }
    // `for (Direction direction : Direction.Plane.HORIZONTAL) { … createChest(10 +
    // stepX*2, -11, 10 + stepZ*2, DESERT_PYRAMID) }` — the four alcove floors, each
    // overwriting the `air` the loop above wrote there. **The iteration order is the
    // specification**, because each chest consumes one `nextLong()`: NORTH, EAST,
    // SOUTH, WEST, i.e. steps (0,-1), (1,0), (0,1), (-1,0).
    for (dx, dz) in [(0, -2), (2, 0), (0, 2), (-2, 0)] {
        b.create_chest(random, 10 + dx, -11, 10 + dz, DESERT_PYRAMID_LOOT);
    }

    // `addCellar`.
    let (rx, ry, rz) = (16, -4, 13);
    // `addCellarStairs`: three counterclockwise-rotated stairs plus the sand slope.
    // `sandStoneStairs.rotate(COUNTERCLOCKWISE_90)` on a default (north-facing)
    // stair is west-facing.
    let ccw = stairs("west");
    b.place(&ccw, 13, -1, 17);
    b.place(&ccw, 14, -2, 17);
    b.place(&ccw, 15, -3, 17);
    // `level.getRandom().nextBoolean()` — position-seeded here; see the fn doc.
    let variant = {
        let mut r = lodestone_worldgen_core::rng::LegacyRandomSource::new(
            lodestone_worldgen_core::rng::get_seed(
                b.world_x(rx, rz),
                b.world_y(ry),
                b.world_z(rx, rz),
            ),
        );
        r.next_bool()
    };
    for dx in -4..=0 {
        b.place(&sand, rx + dx, ry + 4, rz + 4);
    }
    b.place(&sand, rx - 2, ry + 3, rz + 4);
    b.place(if variant { &sand } else { &sandstone }, rx - 1, ry + 3, rz + 4);
    b.place(if variant { &sandstone } else { &sand }, rx, ry + 3, rz + 4);
    b.place(&sand, rx - 1, ry + 2, rz + 4);
    b.place(&sandstone, rx, ry + 2, rz + 4);
    b.place(&sand, rx, ry + 1, rz + 4);

    // `addCellarRoom`. Note `skipAir = true` on every `generateBox` here, which in
    // vanilla means "only overwrite a non-air block". Every one of these positions
    // is inside the sandstone slab written by the very first `generateBox`, so the
    // predicate is true throughout and the boxes are unconditional — which is why
    // the eager list needs no world read for them.
    for (x0, y0, z0, x1, y1, z1, state) in [
        (rx - 3, ry + 1, rz - 3, rx - 3, ry + 1, rz + 2, &cut),
        (rx + 3, ry + 1, rz - 3, rx + 3, ry + 1, rz + 2, &cut),
        (rx - 3, ry + 1, rz - 3, rx + 3, ry + 1, rz - 2, &cut),
        (rx - 3, ry + 1, rz + 3, rx + 3, ry + 1, rz + 3, &cut),
        (rx - 3, ry + 2, rz - 3, rx - 3, ry + 2, rz + 2, &chiseled),
        (rx + 3, ry + 2, rz - 3, rx + 3, ry + 2, rz + 2, &chiseled),
        (rx - 3, ry + 2, rz - 3, rx + 3, ry + 2, rz - 2, &chiseled),
        (rx - 3, ry + 2, rz + 3, rx + 3, ry + 2, rz + 3, &chiseled),
        (rx - 3, -1, rz - 3, rx - 3, -1, rz + 2, &cut),
        (rx + 3, -1, rz - 3, rx + 3, -1, rz + 2, &cut),
        (rx - 3, -1, rz - 3, rx + 3, -1, rz - 2, &cut),
        (rx - 3, -1, rz + 3, rx + 3, -1, rz + 3, &cut),
    ] {
        b.generate_box(x0, y0, z0, x1, y1, z1, state, state);
    }
    // `placeSandBox` / `placeSand` do **not** place anything: they record candidate
    // positions for the structure's `afterPlace` suspicious-sand pass. The blocks
    // that end up there are all written below.
    let mut candidates: Vec<[i32; 3]> = Vec::new();
    for y in (ry + 1)..=(ry + 3) {
        for x in (rx - 2)..=(rx + 2) {
            for z in (rz - 2)..=(rz + 2) {
                candidates.push(b.world_pos(x, y, z));
            }
        }
    }
    // `placeCollapsedRoof(x-2, y+4, z-2, x+2, z+2)`.
    let roof_y = ry + 4;
    for x in (rx - 2)..=(rx + 2) {
        for z in (rz - 2)..=(rz + 2) {
            let pos = b.world_pos(x, roof_y, z);
            let mut r = lodestone_worldgen_core::rng::LegacyRandomSource::new(
                lodestone_worldgen_core::rng::get_seed(pos[0], pos[1], pos[2]),
            );
            let state = if r.next_float() < 0.33 { &sandstone } else { &sand };
            b.place(state, x, roof_y, z);
        }
    }
    // `randomCollapsedRoofPos` — one guaranteed suspicious block in the roof,
    // chosen by a positional fork at the roof's own corner. This one is vanilla's
    // own positional random, not a deviation.
    let roof_corner = b.world_pos(rx - 2, roof_y, rz - 2);
    let collapsed_roof_pos = {
        use lodestone_worldgen_core::rng::{LegacyRandomSource, PositionalRandomFactory};
        let mut r = LegacyRandomSource::new(PYRAMID_ROOF_SEED_PLACEHOLDER)
            .fork_positional()
            .at(roof_corner[0], roof_corner[1], roof_corner[2]);
        let rpx = r.next_int_bounded(5) + (rx - 2);
        let rpz = r.next_int_bounded(5) + (rz - 2);
        b.world_pos(rpx, roof_y, rpz)
    };
    let terracotta_ring = [
        (blue.clone(), 0, 0),
        (orange.clone(), 1, -1),
        (orange.clone(), 1, 1),
        (orange.clone(), -1, -1),
        (orange.clone(), -1, 1),
        (orange.clone(), 2, 0),
        (orange.clone(), -2, 0),
        (orange.clone(), 0, 2),
        (orange.clone(), 0, -2),
        (orange.clone(), 3, 0),
    ];
    for (state, dx, dz) in &terracotta_ring {
        b.place(state, rx + dx, ry, rz + dz);
    }
    candidates.push(b.world_pos(rx + 3, ry + 1, rz));
    candidates.push(b.world_pos(rx + 3, ry + 2, rz));
    b.place(&cut, rx + 4, ry + 1, rz);
    b.place(&chiseled, rx + 4, ry + 2, rz);
    b.place(&orange, rx - 3, ry, rz);
    candidates.push(b.world_pos(rx - 3, ry + 1, rz));
    candidates.push(b.world_pos(rx - 3, ry + 2, rz));
    b.place(&cut, rx - 4, ry + 1, rz);
    b.place(&chiseled, rx - 4, ry + 2, rz);
    b.place(&orange, rx, ry, rz + 3);
    candidates.push(b.world_pos(rx, ry + 1, rz + 3));
    candidates.push(b.world_pos(rx, ry + 2, rz + 3));
    b.place(&orange, rx, ry, rz - 3);
    candidates.push(b.world_pos(rx, ry + 1, rz - 3));
    candidates.push(b.world_pos(rx, ry + 2, rz - 3));
    b.place(&cut, rx, ry + 1, rz - 4);
    b.place(&chiseled, rx, -2, rz - 4);

    let mut piece = b.finish("minecraft:tedp");
    after_place_suspicious_sand(&mut piece, &candidates, collapsed_roof_pos);
    vec![piece]
}

/// `DesertPyramidPiece.WIDTH`. Public because `SinglePieceStructure`'s
/// `findGenerationPoint` samples the four corner heights of this footprint
/// *before* any piece exists, and refuses the start outright when the lowest is
/// below sea level.
pub const PYRAMID_WIDTH: i32 = 21;
/// `DesertPyramidPiece.DEPTH`.
pub const PYRAMID_DEPTH: i32 = 21;

/// A stand-in for `level.getSeed()` inside the pyramid's roof pick.
///
/// `randomCollapsedRoofPos` forks the **world** seed positionally, and the piece
/// generator does not have it: a start predicate is handed `seed` but the roof pick
/// happens inside `postProcess`, three layers down. Using a fixed value here makes
/// the pick a pure function of position, which is the property that matters (it is
/// one block in a 5×5 patch that is already ~⅓ sandstone and ⅔ sand); threading the
/// world seed down would be the faithful version and is the one thing in this
/// generator that is knowingly seed-independent. Recorded as
/// `coded:pyramid_roof_seed` on the ledger.
const PYRAMID_ROOF_SEED_PLACEHOLDER: i64 = 0;

/// `DesertPyramidStructure.afterPlace` — turn some of the cellar's recorded sand
/// candidates into suspicious sand and the rest into plain sand.
///
/// Chunk-independent in vanilla already: the candidate set is the whole piece's,
/// the shuffle is a positional fork at the piece box's centre, and only the
/// *writes* are clipped by `chunkBB`. So it maps onto the eager model directly, and
/// the blocks it produces are appended to the piece's list — after everything the
/// piece itself wrote, which is what makes them win.
fn after_place_suspicious_sand(
    piece: &mut StructurePiece,
    candidates: &[[i32; 3]],
    collapsed_roof_pos: [i32; 3],
) {
    use lodestone_worldgen_core::rng::{LegacyRandomSource, PositionalRandomFactory};
    let suspicious = "minecraft:suspicious_sand[dusted=0]";
    let plain = "minecraft:sand";
    let mut out: Vec<CodedBlock> = Vec::new();
    // `placeSuspiciousSand(chunkBB, level, getRandomCollapsedRoofPos())` runs
    // *before* the shuffled walk, so a roof position that is also a candidate is
    // overwritten by the walk's verdict. Order preserved.
    out.push(CodedBlock {
        pos: collapsed_roof_pos,
        state: suspicious.to_string(),
    });
    // `SortedArraySet.create(Vec3i::compareTo)` — unique, and sorted by
    // `Vec3i.compareTo`, which orders by **y, then z, then x**. The order is the
    // specification: it is the list the shuffle permutes.
    let mut unique: Vec<[i32; 3]> = candidates.to_vec();
    unique.sort_by_key(|p| (p[1], p[2], p[0]));
    unique.dedup();
    let centre = {
        let b = piece.bounding_box;
        [
            b.min[0] + (b.max[0] - b.min[0] + 1) / 2,
            b.min[1] + (b.max[1] - b.min[1] + 1) / 2,
            b.min[2] + (b.max[2] - b.min[2] + 1) / 2,
        ]
    };
    let mut random = LegacyRandomSource::new(PYRAMID_ROOF_SEED_PLACEHOLDER)
        .fork_positional()
        .at(centre[0], centre[1], centre[2]);
    super::pool::shuffle(&mut unique, &mut random);
    // `positionalRandom.nextInt(5, 8)` is `origin + nextInt(bound - origin)`, i.e.
    // 5..=7 — **not** `nextIntBetweenInclusive(5, 8)`, which would be 5..=8.
    let mut to_place = i32::min(
        i32::try_from(unique.len()).unwrap_or(i32::MAX),
        random.next_int_bounded(3) + 5,
    );
    for pos in unique {
        let state = if to_place > 0 {
            to_place -= 1;
            suspicious
        } else {
            plain
        };
        out.push(CodedBlock {
            pos,
            state: state.to_string(),
        });
    }
    let blocks = piece.blocks.take().map(|b| (*b).clone()).unwrap_or_default();
    let mut blocks = blocks;
    blocks.extend(out);
    piece.blocks = Some(Arc::new(blocks));
}

/// `JungleTemplePiece.WIDTH`. Public for the same reason
/// [`PYRAMID_WIDTH`] is: `SinglePieceStructure.findGenerationPoint` samples this
/// footprint's four corners *before* any piece exists.
pub const JUNGLE_WIDTH: i32 = 12;
/// `JungleTemplePiece.DEPTH`.
pub const JUNGLE_DEPTH: i32 = 15;

/// `JungleTempleStructure` → `JungleTemplePiece` — the whole temple, its two
/// tripwire/dispenser traps and its piston puzzle.
///
/// # The one deviation, and why the draw *count* still matches
///
/// `postProcess`'s `random` is the **decorating chunk's** feature stream, not the
/// start's, and every one of this piece's draws comes from it: **1,522**
/// `MossStoneSelector.nextFloat()` calls (the summed volume of the 43 selector
/// `generateBox` call sites, loops expanded) plus four `nextLong()`s for the two
/// chests and two dispensers. Vanilla is therefore chunk-order dependent here in exactly
/// the way §12.139 records for `desert_pyramid` — a temple spanning two chunks gets
/// its cobble/mossy pattern from whichever chunk's stream reached each block, and
/// the four `placed*` booleans mean the container draws happen in whichever pass
/// first had the container inside `chunkBB`.
///
/// Resolved the same way `swamp_hut`'s sink and the beached shipwreck's
/// `nextInt(3)` were: the draws come out of the **structure's own per-chunk
/// stream**, continuing after the orientation draw, in source order. That stream is
/// per-structure-per-chunk and nothing reads it after this call, so no other
/// structure's draws move, and the whole temple becomes a pure function of
/// `(seed, chunk)`. Ledgered as `coded:decoration_random`.
///
/// The *number* and *order* of draws is vanilla's, which is the half that a wrong
/// implementation gets wrong silently: a box whose selector is only consulted for
/// positions inside the served chunk still produces a plausible temple.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn jungle_pyramid_pieces<R: RandomSource>(
    cx: i32,
    cz: i32,
    ctx: &dyn StartContext,
    random: &mut R,
) -> Vec<StructurePiece> {
    // `getRandomHorizontalDirection(random)` in the piece constructor, from
    // `context.random()` — the one draw vanilla also takes from this stream.
    let orientation = Facing::random(random);
    let w = JUNGLE_WIDTH;
    let d = JUNGLE_DEPTH;
    let mut b = Builder::new(cx * 16, 64, cz * 16, orientation, w, 10, d);
    if !b.average_ground_height(ctx, 0) {
        return Vec::new();
    }

    let cobble = s("minecraft:cobblestone");
    let mossy = s("minecraft:mossy_cobblestone");
    let air = s("minecraft:air");
    // `MossStoneSelector.next`: one `nextFloat()` per position, ignoring the
    // coordinates and the edge flag entirely.
    let mut moss = |r: &mut R, _x: i32, _y: i32, _z: i32, _edge: bool| {
        if r.next_float() < 0.4 {
            cobble.clone()
        } else {
            mossy.clone()
        }
    };
    // One name per `generateBox(level, chunkBB, …, false, random, STONE_SELECTOR)`,
    // so the transcription below stays line-for-line with the Java.
    macro_rules! moss_box {
        ($x0:expr, $y0:expr, $z0:expr, $x1:expr, $y1:expr, $z1:expr) => {
            b.generate_box_selected($x0, $y0, $z0, $x1, $y1, $z1, random, &mut moss)
        };
    }

    moss_box!(0, -4, 0, w - 1, 0, d - 1);
    moss_box!(2, 1, 2, 9, 2, 2);
    moss_box!(2, 1, 12, 9, 2, 12);
    moss_box!(2, 1, 3, 2, 2, 11);
    moss_box!(9, 1, 3, 9, 2, 11);
    moss_box!(1, 3, 1, 10, 6, 1);
    moss_box!(1, 3, 13, 10, 6, 13);
    moss_box!(1, 3, 2, 1, 6, 12);
    moss_box!(10, 3, 2, 10, 6, 12);
    moss_box!(2, 3, 2, 9, 3, 12);
    moss_box!(2, 6, 2, 9, 6, 12);
    moss_box!(3, 7, 3, 8, 7, 11);
    moss_box!(4, 8, 4, 7, 8, 10);
    b.generate_air_box(3, 1, 3, 8, 2, 11);
    b.generate_air_box(4, 3, 6, 7, 3, 9);
    b.generate_air_box(2, 4, 2, 9, 5, 12);
    b.generate_air_box(4, 6, 5, 7, 6, 9);
    b.generate_air_box(5, 7, 6, 6, 7, 8);
    b.generate_air_box(5, 1, 2, 6, 2, 2);
    b.generate_air_box(5, 2, 12, 6, 2, 12);
    b.generate_air_box(5, 5, 1, 6, 5, 1);
    b.generate_air_box(5, 5, 13, 6, 5, 13);
    b.place(&air, 1, 5, 5);
    b.place(&air, 10, 5, 5);
    b.place(&air, 1, 5, 9);
    b.place(&air, 10, 5, 9);

    let mut z = 0;
    while z <= 14 {
        moss_box!(2, 4, z, 2, 5, z);
        moss_box!(4, 4, z, 4, 5, z);
        moss_box!(7, 4, z, 7, 5, z);
        moss_box!(9, 4, z, 9, 5, z);
        z += 14;
    }
    moss_box!(5, 6, 0, 6, 6, 0);

    let mut x = 0;
    while x <= 11 {
        let mut z = 2;
        while z <= 12 {
            moss_box!(x, 4, z, x, 5, z);
            z += 2;
        }
        moss_box!(x, 6, 5, x, 6, 5);
        moss_box!(x, 6, 9, x, 6, 9);
        x += 11;
    }

    moss_box!(2, 7, 2, 2, 9, 2);
    moss_box!(9, 7, 2, 9, 9, 2);
    moss_box!(2, 7, 12, 2, 9, 12);
    moss_box!(9, 7, 12, 9, 9, 12);
    moss_box!(4, 9, 4, 4, 9, 4);
    moss_box!(7, 9, 4, 7, 9, 4);
    moss_box!(4, 9, 10, 4, 9, 10);
    moss_box!(7, 9, 10, 7, 9, 10);
    moss_box!(5, 9, 7, 6, 9, 7);

    let stairs = |facing: &str| {
        s(&format!(
            "minecraft:cobblestone_stairs[facing={facing},half=bottom,shape=straight,\
             waterlogged=false]"
        ))
    };
    let east_stairs = stairs("east");
    let west_stairs = stairs("west");
    let south_stairs = stairs("south");
    let north_stairs = stairs("north");
    b.place(&north_stairs, 5, 9, 6);
    b.place(&north_stairs, 6, 9, 6);
    b.place(&south_stairs, 5, 9, 8);
    b.place(&south_stairs, 6, 9, 8);
    b.place(&north_stairs, 4, 0, 0);
    b.place(&north_stairs, 5, 0, 0);
    b.place(&north_stairs, 6, 0, 0);
    b.place(&north_stairs, 7, 0, 0);
    b.place(&north_stairs, 4, 1, 8);
    b.place(&north_stairs, 4, 2, 9);
    b.place(&north_stairs, 4, 3, 10);
    b.place(&north_stairs, 7, 1, 8);
    b.place(&north_stairs, 7, 2, 9);
    b.place(&north_stairs, 7, 3, 10);
    moss_box!(4, 1, 9, 4, 1, 9);
    moss_box!(7, 1, 9, 7, 1, 9);
    moss_box!(4, 1, 10, 7, 2, 10);
    moss_box!(5, 4, 5, 6, 4, 5);
    b.place(&east_stairs, 4, 4, 5);
    b.place(&west_stairs, 7, 4, 5);

    for i in 0..4 {
        b.place(&south_stairs, 5, -i, 6 + i);
        b.place(&south_stairs, 6, -i, 6 + i);
        b.generate_air_box(5, -i, 7 + i, 6, -i, 9 + i);
    }

    b.generate_air_box(1, -3, 12, 10, -1, 13);
    b.generate_air_box(1, -3, 1, 3, -1, 13);
    b.generate_air_box(1, -3, 1, 9, -1, 5);

    let mut z = 1;
    while z <= 13 {
        moss_box!(1, -3, z, 1, -2, z);
        z += 2;
    }
    let mut z = 2;
    while z <= 12 {
        moss_box!(1, -1, z, 3, -1, z);
        z += 2;
    }
    moss_box!(2, -2, 1, 5, -2, 1);
    moss_box!(7, -2, 1, 9, -2, 1);
    moss_box!(6, -3, 1, 6, -3, 1);
    moss_box!(6, -1, 1, 6, -1, 1);

    // `attached=true` is set explicitly on every hook and wire; `powered` and
    // `disarmed` keep their defaults.
    let hook = |facing: &str| {
        s(&format!(
            "minecraft:tripwire_hook[attached=true,facing={facing},powered=false]"
        ))
    };
    let tripwire_ew = s(
        "minecraft:tripwire[attached=true,disarmed=false,east=true,north=false,powered=false,\
         south=false,west=true]",
    );
    let tripwire_ns = s(
        "minecraft:tripwire[attached=true,disarmed=false,east=false,north=true,powered=false,\
         south=true,west=false]",
    );
    b.place(&hook("east"), 1, -3, 8);
    b.place(&hook("west"), 4, -3, 8);
    b.place(&tripwire_ew, 2, -3, 8);
    b.place(&tripwire_ew, 3, -3, 8);

    let wire = |east: &str, north: &str, south: &str, west: &str| {
        s(&format!(
            "minecraft:redstone_wire[east={east},north={north},power=0,south={south},west={west}]"
        ))
    };
    let wire_ns = wire("none", "side", "side", "none");
    b.place(&wire_ns, 5, -3, 7);
    b.place(&wire_ns, 5, -3, 6);
    b.place(&wire_ns, 5, -3, 5);
    b.place(&wire_ns, 5, -3, 4);
    b.place(&wire_ns, 5, -3, 3);
    b.place(&wire_ns, 5, -3, 2);
    b.place(&wire("none", "side", "none", "side"), 5, -3, 1);
    b.place(&wire("side", "none", "none", "side"), 4, -3, 1);
    b.place(&mossy.clone(), 3, -3, 1);
    b.create_dispenser(random, 3, -2, 1, "north", JUNGLE_TEMPLE_DISPENSER_LOOT);
    b.place(
        &s("minecraft:vine[east=false,north=false,south=true,up=false,west=false]"),
        3,
        -2,
        2,
    );
    b.place(&hook("north"), 7, -3, 1);
    b.place(&hook("south"), 7, -3, 5);
    b.place(&tripwire_ns, 7, -3, 2);
    b.place(&tripwire_ns, 7, -3, 3);
    b.place(&tripwire_ns, 7, -3, 4);
    b.place(&wire("side", "none", "none", "side"), 8, -3, 6);
    b.place(&wire("none", "none", "side", "side"), 9, -3, 6);
    b.place(&wire("none", "side", "up", "none"), 9, -3, 5);
    b.place(&mossy.clone(), 9, -3, 4);
    b.place(&wire_ns, 9, -2, 4);
    b.create_dispenser(random, 9, -2, 3, "west", JUNGLE_TEMPLE_DISPENSER_LOOT);
    let vine_east = s("minecraft:vine[east=true,north=false,south=false,up=false,west=false]");
    b.place(&vine_east, 8, -1, 3);
    b.place(&vine_east, 8, -2, 3);
    b.create_chest(random, 8, -3, 3, JUNGLE_TEMPLE_LOOT);
    for (x, y, z) in [
        (9, -3, 2),
        (8, -3, 1),
        (4, -3, 5),
        (5, -2, 5),
        (5, -1, 5),
        (6, -3, 5),
        (7, -2, 5),
        (7, -1, 5),
        (8, -3, 5),
    ] {
        b.place(&mossy.clone(), x, y, z);
    }
    moss_box!(9, -1, 1, 9, -1, 5);
    b.generate_air_box(8, -3, 8, 10, -1, 10);
    let chiseled = s("minecraft:chiseled_stone_bricks");
    b.place(&chiseled, 8, -2, 11);
    b.place(&chiseled, 9, -2, 11);
    b.place(&chiseled, 10, -2, 11);
    let lever = s("minecraft:lever[face=wall,facing=north,powered=false]");
    b.place(&lever, 8, -2, 12);
    b.place(&lever, 9, -2, 12);
    b.place(&lever, 10, -2, 12);
    moss_box!(8, -3, 8, 8, -3, 10);
    moss_box!(10, -3, 8, 10, -3, 10);
    b.place(&mossy.clone(), 10, -2, 9);
    b.place(&wire_ns, 8, -2, 9);
    b.place(&wire_ns, 8, -2, 10);
    b.place(&wire("side", "side", "side", "side"), 10, -1, 9);
    b.place(&s("minecraft:sticky_piston[extended=false,facing=up]"), 9, -2, 8);
    b.place(&s("minecraft:sticky_piston[extended=false,facing=west]"), 10, -2, 8);
    b.place(&s("minecraft:sticky_piston[extended=false,facing=west]"), 10, -1, 8);
    b.place(
        &s("minecraft:repeater[delay=1,facing=north,locked=false,powered=false]"),
        10,
        -2,
        10,
    );
    b.create_chest(random, 9, -3, 10, JUNGLE_TEMPLE_LOOT);

    vec![b.finish("minecraft:tejp")]
}

/// `BuiltInLootTables.JUNGLE_TEMPLE`.
const JUNGLE_TEMPLE_LOOT: &str = "minecraft:chests/jungle_temple";
/// `BuiltInLootTables.JUNGLE_TEMPLE_DISPENSER`.
const JUNGLE_TEMPLE_DISPENSER_LOOT: &str = "minecraft:chests/jungle_temple_dispenser";
/// `BuiltInLootTables.DESERT_PYRAMID`.
const DESERT_PYRAMID_LOOT: &str = "minecraft:chests/desert_pyramid";

#[cfg(test)]
mod tests {
    use super::*;

    /// A [`StartContext`] over flat terrain at a known height.
    struct Flat(i32);
    impl StartContext for Flat {
        fn first_occupied_height(&self, _x: i32, _z: i32, _heightmap: HeightmapKind) -> i32 {
            self.0
        }
        fn biome_at_quart(&self, _qx: i32, _qy: i32, _qz: i32) -> String {
            "minecraft:desert".to_string()
        }
        fn sea_level(&self) -> i32 {
            63
        }
    }

    /// `makeBoundingBox`'s axis swap, and `getWorldX/Z`'s four orientation cases.
    ///
    /// The failure this excludes is the one with no visible symptom in a
    /// screenshot: an orientation whose coordinate mapping is wrong still builds a
    /// complete, plausible pyramid — rotated or mirrored the wrong way, in a box
    /// that no longer matches its own `BB`.
    #[test]
    fn the_orientation_transform_maps_local_to_world_per_direction() {
        // A 7-wide, 9-deep piece at (100, 64, 200).
        let north = Builder::new(100, 64, 200, Facing::North, 7, 7, 9);
        assert_eq!(north.bounding_box().min, [100, 64, 200]);
        assert_eq!(north.bounding_box().max, [106, 70, 208]);
        // NORTH: x counts up from minX, z counts **down** from maxZ.
        assert_eq!(north.world_pos(0, 0, 0), [100, 64, 208]);
        assert_eq!(north.world_pos(6, 0, 8), [106, 64, 200]);

        let south = Builder::new(100, 64, 200, Facing::South, 7, 7, 9);
        assert_eq!(south.bounding_box().max, [106, 70, 208]);
        assert_eq!(south.world_pos(0, 0, 0), [100, 64, 200]);
        assert_eq!(south.world_pos(6, 0, 8), [106, 64, 208]);

        // An X-axis orientation swaps the box's own extents: 9 along x, 7 along z.
        let east = Builder::new(100, 64, 200, Facing::East, 7, 7, 9);
        assert_eq!(east.bounding_box().max, [108, 70, 206]);
        assert_eq!(east.world_pos(0, 0, 0), [100, 64, 200]);
        assert_eq!(east.world_pos(6, 0, 8), [108, 64, 206]);

        let west = Builder::new(100, 64, 200, Facing::West, 7, 7, 9);
        assert_eq!(west.bounding_box().max, [108, 70, 206]);
        assert_eq!(west.world_pos(0, 0, 0), [108, 64, 200]);
        assert_eq!(west.world_pos(6, 0, 8), [100, 64, 206]);
    }

    /// `Facing::random` is one draw over `{NORTH, EAST, SOUTH, WEST}` and the 2D
    /// data values are vanilla's — `SOUTH` is 0, not `NORTH`.
    #[test]
    fn facing_random_is_one_draw_in_plane_order() {
        use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom};
        for (index, expected) in [
            (0, Facing::North),
            (1, Facing::East),
            (2, Facing::South),
            (3, Facing::West),
        ] {
            // Find a seed whose first `nextInt(4)` is `index`, then check the map.
            let mut seed = 0i64;
            loop {
                let mut probe = WorldgenRandom::new(LegacyRandomSource::new(seed));
                if probe.next_int_bounded(4) == index {
                    break;
                }
                seed += 1;
            }
            let mut random = WorldgenRandom::new(LegacyRandomSource::new(seed));
            assert_eq!(Facing::random(&mut random), expected, "index {index}");
        }
        assert_eq!(Facing::South.data_2d(), 0);
        assert_eq!(Facing::West.data_2d(), 1);
        assert_eq!(Facing::North.data_2d(), 2);
        assert_eq!(Facing::East.data_2d(), 3);
    }

    /// `generateBox`'s edge/fill split: a 3×3×3 box is 26 edge blocks and one
    /// interior, and a 1-thick box is entirely edge.
    #[test]
    fn generate_box_splits_edge_from_fill() {
        let mut b = Builder::new(0, 64, 0, Facing::North, 8, 8, 8);
        b.generate_box(
            0,
            0,
            0,
            2,
            2,
            2,
            &s("minecraft:stone"),
            &s("minecraft:dirt"),
        );
        assert_eq!(b.len(), 27);
        let dirt = b.blocks.iter().filter(|x| x.state == "minecraft:dirt").count();
        assert_eq!(dirt, 1, "a 3-cube has exactly one interior cell");

        let mut flat = Builder::new(0, 64, 0, Facing::North, 8, 8, 8);
        flat.generate_box(
            0,
            0,
            0,
            4,
            0,
            4,
            &s("minecraft:stone"),
            &s("minecraft:dirt"),
        );
        assert_eq!(flat.len(), 25);
        assert!(
            flat.blocks.iter().all(|x| x.state == "minecraft:stone"),
            "a 1-thick box has no interior"
        );
    }

    /// The hut is built, and its Y follows the terrain rather than the literal 64.
    #[test]
    fn a_swamp_hut_places_its_planks_on_the_ground() {
        use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom};
        let ctx = Flat(71);
        let mut random = WorldgenRandom::new(LegacyRandomSource::new(4));
        let pieces = swamp_hut_pieces(3, -7, &ctx, &mut random);
        assert_eq!(pieces.len(), 1);
        let piece = &pieces[0];
        let blocks = piece.blocks.as_ref().expect("a coded piece carries blocks");
        // `free_height` is `first_occupied + 1` = 72, and the offset is 0.
        assert_eq!(piece.bounding_box.min[1], 72);
        assert!(
            blocks.iter().any(|b| b.state == "minecraft:spruce_planks"),
            "no planks"
        );
        assert!(blocks.iter().any(|b| b.state.starts_with("minecraft:spruce_stairs")));
        assert!(blocks.iter().any(|b| b.state == "minecraft:cauldron"));
        // Every block is inside the piece's own box — the invariant the clip
        // depends on, and the one an orientation bug breaks.
        for block in blocks.iter() {
            let bb = piece.bounding_box;
            assert!(
                (0..3).all(|i| block.pos[i] >= bb.min[i] && block.pos[i] <= bb.max[i]),
                "{:?} is outside {:?}",
                block.pos,
                bb
            );
        }
    }

    /// The pyramid is built and its `afterPlace` pass places **exactly** 5–7
    /// suspicious sand blocks plus the one guaranteed roof block.
    ///
    /// A predicted count, not a direction: `nextInt(5, 8)` is `5 + nextInt(3)`, so
    /// 8 is the wrong upper bound and `nextIntBetweenInclusive` would allow it.
    #[test]
    fn a_desert_pyramid_places_its_layers_and_a_bounded_suspicious_sand_count() {
        use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom};
        let ctx = Flat(74);
        for seed in 0..12i64 {
            let mut random = WorldgenRandom::new(LegacyRandomSource::new(seed));
            let pieces = desert_pyramid_pieces(0, 0, &ctx, &mut random);
            assert_eq!(pieces.len(), 1);
            let blocks = pieces[0].blocks.as_ref().expect("blocks");
            assert!(
                blocks.iter().any(|b| b.state == "minecraft:sandstone"),
                "no sandstone"
            );
            assert!(blocks.iter().any(|b| b.state.starts_with("minecraft:tnt")));
            assert!(blocks.iter().any(|b| b.state == "minecraft:blue_terracotta"));
            // Last-write-wins, so count the *final* state per position.
            let mut final_state: std::collections::HashMap<[i32; 3], &str> =
                std::collections::HashMap::new();
            for block in blocks.iter() {
                final_state.insert(block.pos, block.state.as_str());
            }
            let suspicious = final_state
                .values()
                .filter(|s| s.starts_with("minecraft:suspicious_sand"))
                .count();
            assert!(
                (5..=8).contains(&suspicious),
                "seed {seed}: {suspicious} suspicious sand blocks, want 5..=8 \
                 (5..=7 from the walk plus at most one distinct roof block)"
            );
        }
    }

    /// The jungle temple consumes **exactly 1,531** primitive draws, and its
    /// cobble/mossy split lands on 0.4 rather than on 0.6.
    ///
    /// # Both numbers come from outside this module
    ///
    /// The draw count was derived by a script over
    /// `.cache/mc/26.2/.../JungleTemplePiece.java` — 43 `generateBox(…, false,
    /// random, STONE_SELECTOR)` call sites, loops expanded, summed volume **1,522**
    /// — plus one `nextInt(4)` for the orientation and four `nextLong()`s for the
    /// two chests and two dispensers. `WorldgenRandom::count()` counts `next(bits)`
    /// calls, and a legacy `nextLong()` is two of them, so `1 + 1522 + 8 = 1531`.
    ///
    /// **This is the assertion that a plausible temple cannot satisfy by accident.**
    /// Skipping the selector for a position outside the served chunk, shuffling a
    /// raw pool instead of its weight-expanded copy, or re-seeding between the two
    /// halves all build something temple-shaped; only a stream-position assertion
    /// sees them (§12.135).
    ///
    /// The ratio is the *magnitude* discipline: `nextFloat() < 0.4F` selects
    /// **cobblestone**, so the inverted reading yields 0.6, and both hypotheses are
    /// computed here rather than the sign of the difference being asserted. One
    /// continuous stream of 1,522 draws, not 1,522 fresh randoms — sequentially
    /// seeded LCGs are correlated in their first draw and biased two earlier rate
    /// measurements by several sigma (§12.139).
    #[test]
    fn the_jungle_temple_draw_count_and_moss_ratio_are_vanillas() {
        use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom};
        let ctx = Flat(74);
        let mut random = WorldgenRandom::new(LegacyRandomSource::new(-195_764_831));
        let pieces = jungle_pyramid_pieces(0, 0, &ctx, &mut random);
        assert_eq!(pieces.len(), 1);
        assert_eq!(
            random.count(),
            1_531,
            "the temple's stream position is the specification"
        );

        let blocks = pieces[0].blocks.as_ref().expect("blocks");
        // Every *write*, not the final state per position: the draws happened once
        // per write and last-write-wins would hide most of them.
        let cobble = blocks.iter().filter(|b| b.state == "minecraft:cobblestone").count();
        let mossy = blocks
            .iter()
            .filter(|b| b.state == "minecraft:mossy_cobblestone")
            .count();
        // 12 `placeBlock(MOSSY_COBBLESTONE, …)` statements are unconditional and are
        // *not* selector draws, so the predicted fraction is 0.4·n/(n+12).
        assert_eq!(cobble + mossy, 1_522 + 12, "cobble {cobble} + mossy {mossy}");
        let fraction = cobble as f64 / (cobble + mossy) as f64;
        let correct = 0.4 * 1_522.0 / 1_534.0;
        let inverted = 0.6 * 1_522.0 / 1_534.0;
        assert!(
            (fraction - correct).abs() < 0.04,
            "cobble fraction {fraction:.4}; correct hypothesis {correct:.4}, \
             inverted-comparison hypothesis {inverted:.4}"
        );
        assert!(
            (fraction - inverted).abs() > 0.1,
            "the measurement must exclude the inverted hypothesis, not merely admit \
             the correct one: {fraction:.4} vs {inverted:.4}"
        );

        // The two traps and the two chests, with vanilla's own table ids.
        let tables: Vec<&str> = pieces[0].loot.iter().map(|l| l.table.as_str()).collect();
        assert_eq!(
            tables,
            vec![
                JUNGLE_TEMPLE_DISPENSER_LOOT,
                JUNGLE_TEMPLE_DISPENSER_LOOT,
                JUNGLE_TEMPLE_LOOT,
                JUNGLE_TEMPLE_LOOT,
            ]
        );
        // 12 x 10 x 15 for a Z-axis orientation, and it never fails to place.
        let bb = pieces[0].bounding_box;
        let extents = [
            bb.max[0] - bb.min[0] + 1,
            bb.max[1] - bb.min[1] + 1,
            bb.max[2] - bb.min[2] + 1,
        ];
        assert!(
            extents == [12, 10, 15] || extents == [15, 10, 12],
            "box extents {extents:?}"
        );
    }

    /// A pyramid is a pure function of `(chunk, terrain)` — the property the
    /// per-chunk clip depends on, and the one vanilla's `level.getRandom()` cellar
    /// draws do **not** have.
    #[test]
    fn a_coded_piece_is_reproducible_block_for_block() {
        use lodestone_worldgen_core::rng::{LegacyRandomSource, WorldgenRandom};
        let ctx = Flat(74);
        let build = || {
            let mut random = WorldgenRandom::new(LegacyRandomSource::new(-195_764_831));
            let pieces = desert_pyramid_pieces(-4, 9, &ctx, &mut random);
            pieces[0]
                .blocks
                .as_ref()
                .map(|b| (**b).clone())
                .unwrap_or_default()
        };
        let first = build();
        let second = build();
        assert_eq!(first.len(), second.len());
        for (a, b) in first.iter().zip(second.iter()) {
            assert_eq!((a.pos, &a.state), (b.pos, &b.state));
        }
        assert!(first.len() > 3_000, "a pyramid is {} blocks", first.len());
    }
}
