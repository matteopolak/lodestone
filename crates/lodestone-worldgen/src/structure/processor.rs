//! Template **processors** — the per-block filters vanilla runs between a
//! template's palette and the world.
//!
//! # What it is
//!
//! The `StructureProcessor` kinds the structures wired here actually name:
//! `BlockIgnoreProcessor` (drop air / structure blocks), `BlockRotProcessor`
//! (integrity — the reason a ruin is a ruin), `RuleProcessor` (match-and-replace
//! block states), `ProtectedBlockProcessor`, `JigsawReplacementProcessor` (a
//! jigsaw block becomes its own `final_state`) and `GravityProcessor` (a
//! `terrain_matching` pool element's blocks follow the surface).
//!
//! # How it works
//!
//! Every processor that needs randomness derives it from the block's **absolute
//! position**: `RandomSource.create(Mth.getSeed(pos))`, a fresh legacy LCG per
//! block. There is no shared stream, so no draw order to preserve across chunks —
//! which is the whole reason integrity survives our per-chunk pipeline. Getting
//! this wrong (a per-piece stream) would make the same shipwreck rot differently
//! in each chunk it spans.
//!
//! A processor sees a [`ProcessedBlock`] (position + state) plus a
//! [`ProcessCtx`] (the block's template-local position, its retained `nbt`, and
//! read access to the world), and returns the block it wants placed or `None` to
//! drop it. **Two of them use more than the state**: `JigsawReplacement` reads
//! `nbt`, `Gravity` rewrites the *position*, and `Rule`'s `location_predicate`
//! reads the world. That is why
//! [`StructureTemplate::place`](super::template::StructureTemplate::place)
//! processes the whole block list before writing any of it.
//!
//! # How to change it
//!
//! * `Processor::process` returning `None` means **drop this block**, exactly as
//!   vanilla's `@Nullable StructureBlockInfo` does. It is not an error path.
//! * A new `processor_type` belongs in [`ProcessorList::parse`](super::pool::ProcessorList),
//!   and anything it does not cover must be named in
//!   [`super::StructureRegistry::unsupported`] — the ledger, not a silent skip.
//! * **`capped` is the one processor that is not per-block** ([`Processor::Capped`]).
//!   It runs in `finalizeProcessing`, over the **whole** processed list, and its
//!   `evaluatesEntirePieceState` flips vanilla's `processOnlyInCurrentChunk` off —
//!   so a piece carrying one is processed over its entire footprint and only
//!   clipped at write time. That is not an optimisation detail: the shuffled index
//!   walk is over the full list, so clipping first would put a different number of
//!   suspicious blocks in each chunk a trail-ruins house spans.
//! * `block_entity_modifier` is honoured only as far as the **block state** goes:
//!   `append_loot` selects the state (`suspicious_gravel`) and the loot table it
//!   appends needs block entities in worldgen, which is ledgered under
//!   `block_entity:append_loot`. Any other modifier type is still refused.

use std::collections::HashSet;
use std::sync::Arc;

use lodestone_worldgen_core::rng::{
    LegacyRandomSource, PositionalRandomFactory, RandomSource, get_seed,
};

use super::template::{BlockNbt, BlockState, nbt_string};
use crate::dense_grid::DenseBlockGrid;

/// Read access to the world a processor is placing into.
///
/// Narrow on purpose: a `location_predicate` asks for one block state at one
/// position and nothing else, so this is the whole surface a processor needs and
/// the whole surface a hermetic test has to fake.
pub trait WorldRead {
    /// The canonical state string at `(x, y, z)`.
    fn block_at(&self, x: i32, y: i32, z: i32) -> &str;
}

impl WorldRead for DenseBlockGrid {
    fn block_at(&self, x: i32, y: i32, z: i32) -> &str {
        self.get(x, y, z)
    }
}

/// One block on its way from a template palette into the world.
///
/// `PartialEq` is load-bearing rather than convenient: [`Processor::Capped`]
/// counts a delegate application as "replaced" only when the returned block
/// **differs** (`!processedBlockInfo.equals(maybeAltered)`), so a delegate that
/// matches but changes nothing consumes an index without consuming the cap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessedBlock {
    /// The **absolute** world position. A `GravityProcessor` rewrites this.
    pub pos: [i32; 3],
    /// The state, still unrotated (`mirror().rotate()` happens after the chain).
    pub state: BlockState,
}

/// Everything a processor can see besides the block itself.
#[allow(missing_debug_implementations)]
pub struct ProcessCtx<'a> {
    /// `templateRelativePos` — the block's unrotated, template-local position.
    /// `GravityProcessor` reads its `delta` from this and nothing else does.
    pub local: [i32; 3],
    /// `referencePos` — `StructureStart.placeInChunk`'s
    /// `(centre.x, pieces[0].box.minY, centre.z)`, where `centre` is the **first**
    /// piece's box centre. A whole-*start* fact, not a per-piece one, which is why
    /// it arrives here rather than living in [`super::template::PlaceSettings`].
    /// Only a [`PosTest::AxisAlignedLinear`] reads it.
    pub reference: [i32; 3],
    /// The block's retained `nbt` compound, if it had one.
    pub nbt: Option<&'a BlockNbt>,
    /// The world as it stands *before* this template writes anything.
    pub world: &'a dyn WorldRead,
}

/// A `RuleTest` — the input or location half of a [`ProcessorRule`].
#[derive(Debug, Clone)]
pub enum RuleTest {
    /// `always_true`. Note vanilla overrides `testAgainstWorldState` to return
    /// true **without** consulting the state or drawing — so an `always_true`
    /// location predicate costs no draw, and adding one would shift every later
    /// rule's roll.
    AlwaysTrue,
    /// `block_match` — the block id, ignoring properties.
    BlockMatch(String),
    /// `blockstate_match` — the exact canonical state.
    BlockStateMatch(String),
    /// `random_block_match`. The draw happens **only if the id matches**
    /// (Java's `&&` short-circuits), which is the difference between a rule list
    /// that rots 20% of cobblestone and one whose later rules see a shifted
    /// stream.
    RandomBlockMatch(String, f32),
    /// `random_blockstate_match`.
    RandomBlockStateMatch(String, f32),
    /// `tag_match`, resolved to its block-id closure at parse time.
    TagMatch(Arc<HashSet<String>>),
}

impl RuleTest {
    /// `test(state, random)`.
    #[must_use]
    pub fn test(&self, state: &BlockState, random: &mut LegacyRandomSource) -> bool {
        match self {
            Self::AlwaysTrue => true,
            Self::BlockMatch(name) => &state.name == name,
            Self::BlockStateMatch(spec) => state.canonical() == *spec,
            Self::RandomBlockMatch(name, probability) => {
                &state.name == name && random.next_float() < *probability
            }
            Self::RandomBlockStateMatch(spec, probability) => {
                state.canonical() == *spec && random.next_float() < *probability
            }
            Self::TagMatch(ids) => ids.contains(&state.name),
        }
    }

    /// `testAgainstWorldState(level, pos, random)` — the same predicate against
    /// the world's own state, with `always_true`'s no-draw short circuit.
    fn test_world(&self, world: &dyn WorldRead, pos: [i32; 3], random: &mut LegacyRandomSource) -> bool {
        if matches!(self, Self::AlwaysTrue) {
            return true;
        }
        let state = BlockState::parse(world.block_at(pos[0], pos[1], pos[2]));
        self.test(&state, random)
    }
}

/// A `Direction.Axis`, for [`PosTest::AxisAlignedLinear`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    /// `x`.
    X,
    /// `y` — `AxisAlignedLinearPosTest`'s default, and the only one bundled.
    Y,
    /// `z`.
    Z,
}

/// A `PosRuleTest` — the third and last predicate of a [`ProcessorRule`].
#[derive(Debug, Clone, Copy)]
pub enum PosTest {
    /// `always_true` (`PosAlwaysTrueTest`). **Costs no draw**, which is why an
    /// absent `position_predicate` and a present one are not interchangeable.
    AlwaysTrue,
    /// `axis_aligned_linear_pos` — one `nextFloat()` against a chance that ramps
    /// linearly with the distance from the reference position along one axis.
    ///
    /// The only bundled use is `high_rampart` (the bastion's `rampart_degradation`
    /// sibling): `max_chance 0.05`, `max_dist 100`, axis `Y` by default, so a
    /// rampart erodes more the further above the start's floor it sits.
    AxisAlignedLinear {
        /// `min_chance`, the chance at `min_dist` and below.
        min_chance: f32,
        /// `max_chance`, the chance at `max_dist` and above.
        max_chance: f32,
        /// `min_dist`.
        min_dist: i32,
        /// `max_dist`. Vanilla's constructor throws when `min_dist >= max_dist`.
        max_dist: i32,
        /// `axis`.
        axis: Axis,
    },
}

impl PosTest {
    /// `test(inTemplatePos, worldPos, worldReference, random)`.
    ///
    /// The distance is taken along **one** axis only (vanilla multiplies each
    /// component by that axis' unit step and sums the absolute values, which zeroes
    /// the other two), and the truncation to `int` before the lerp is vanilla's.
    #[must_use]
    pub fn test(
        &self,
        _local: [i32; 3],
        world: [i32; 3],
        reference: [i32; 3],
        random: &mut LegacyRandomSource,
    ) -> bool {
        match *self {
            Self::AlwaysTrue => true,
            Self::AxisAlignedLinear {
                min_chance,
                max_chance,
                min_dist,
                max_dist,
                axis,
            } => {
                let component = match axis {
                    Axis::X => 0,
                    Axis::Y => 1,
                    Axis::Z => 2,
                };
                // `(int)(xd + yd + zd)` where two of the three are exactly 0.0f.
                let delta = (world[component] - reference[component]).abs();
                #[allow(clippy::cast_precision_loss)]
                let dist = delta as f32;
                let factor = (dist - min_dist as f32) / ((max_dist - min_dist) as f32);
                let chance = clamped_lerp(factor, min_chance, max_chance);
                random.next_float() <= chance
            }
        }
    }
}

/// `Mth.clampedLerp(factor, min, max)` — **factor first**, which is the opposite
/// of the `lerp(start, end, t)` spelling most APIs use and would silently produce
/// a constant chance if read the other way round.
fn clamped_lerp(factor: f32, min: f32, max: f32) -> f32 {
    if factor < 0.0 {
        min
    } else if factor > 1.0 {
        max
    } else {
        min + factor * (max - min)
    }
}

/// One `ProcessorRule`: match the template state (and optionally the world state
/// at the target and the position), emit `output`.
///
/// All three predicates draw from **one** position-seeded stream, in this order,
/// and Java's `&&` short-circuits — so a rule whose input predicate fails costs
/// only the input predicate's draws. Defaulting an unmodelled `position_predicate`
/// to true would therefore shift every later rule's roll, which is why
/// [`super::pool::PoolStore`] refuses one it does not know rather than ignoring it.
#[derive(Debug, Clone)]
pub struct ProcessorRule {
    /// `input_predicate`, tested against the template's state.
    pub input: RuleTest,
    /// `location_predicate`, tested against the world's state at the target.
    pub location: RuleTest,
    /// `position_predicate`.
    pub position: PosTest,
    /// `output_state`.
    pub output: BlockState,
}

/// Per-column surface heights over a rectangle, precomputed for a
/// `GravityProcessor`.
///
/// # Why this is precomputed and not sampled
///
/// Vanilla's `GravityProcessor` calls `level.getHeight(WORLD_SURFACE_WG, x, z)` at
/// placement time, i.e. against the *decorating chunk's* heightmap. Our pipeline
/// places one piece independently in each chunk it spans, so sampling there would
/// give a piece two different surfaces on two sides of a chunk border and shear it
/// — the same failure mode S2 avoided by resolving piece Y eagerly. So the
/// heights are sampled once, at start time, from the same `_WG` noise columns, and
/// travel with the piece.
#[derive(Debug, Clone)]
pub struct ColumnHeights {
    min_x: i32,
    min_z: i32,
    size_x: i32,
    values: Vec<i32>,
}

impl ColumnHeights {
    /// Builds the table over the inclusive rectangle `[min_x, max_x] ×
    /// [min_z, max_z]`, calling `height` for each column.
    pub fn build(
        min_x: i32,
        min_z: i32,
        max_x: i32,
        max_z: i32,
        mut height: impl FnMut(i32, i32) -> i32,
    ) -> Self {
        let size_x = (max_x - min_x + 1).max(1);
        let size_z = (max_z - min_z + 1).max(1);
        let mut values = Vec::with_capacity((size_x * size_z) as usize);
        for z in 0..size_z {
            for x in 0..size_x {
                values.push(height(min_x + x, min_z + z));
            }
        }
        Self {
            min_x,
            min_z,
            size_x,
            values,
        }
    }

    /// The height at `(x, z)`, clamped to the rectangle's edge.
    ///
    /// Clamping rather than panicking because a rule processor ahead of gravity in
    /// the chain can only ever move a block *within* the piece, but the mirror of
    /// that assumption being wrong is a crash in chunk generation.
    #[must_use]
    pub fn get(&self, x: i32, z: i32) -> i32 {
        if self.values.is_empty() {
            return 0;
        }
        let size_z = i32::try_from(self.values.len()).unwrap_or(1) / self.size_x;
        let lx = (x - self.min_x).clamp(0, self.size_x - 1);
        let lz = (z - self.min_z).clamp(0, (size_z - 1).max(0));
        self.values[(lz * self.size_x + lx) as usize]
    }
}

/// A processor in a piece's chain.
#[derive(Debug, Clone)]
pub enum Processor {
    /// `BlockIgnoreProcessor` — drop a block whose id is in the list.
    ///
    /// The most load-bearing processor in the whole unit: `STRUCTURE_AND_AIR`
    /// drops the template's air, which is why a shipwreck's hull keeps the sand
    /// it is buried in instead of being cleared to an air box.
    BlockIgnore(Vec<String>),
    /// `BlockRotProcessor` — keep a block only when
    /// `random.nextFloat() <= integrity`, optionally restricted to a
    /// `rottable_blocks` set.
    BlockRot {
        /// `rottable_blocks`, resolved to block ids. `None` means "every block is
        /// rottable", which is **not** the same as an empty set: an empty set
        /// would rot nothing, and vanilla's `Optional.empty()` rots everything.
        rottable: Option<Arc<HashSet<String>>>,
        /// `integrity`, `0.0..=1.0`.
        integrity: f32,
    },
    /// `RuleProcessor` — first matching rule replaces the state. One
    /// position-seeded LCG is shared by every rule test for one block, in order.
    Rule(Vec<ProcessorRule>),
    /// `ProtectedBlockProcessor` — drop the template's block when the world
    /// already holds one of `cannot_replace` there.
    ProtectedBlocks(Arc<HashSet<String>>),
    /// `JigsawReplacementProcessor` — a jigsaw block becomes the `final_state`
    /// from its own `nbt`, or is dropped when that is `structure_void`.
    ///
    /// Not optional: without it every placed village keeps its jigsaw blocks,
    /// which are `#minecraft:jigsaw`-textured command blocks in the middle of
    /// every wall.
    JigsawReplacement,
    /// `GravityProcessor(heightmap, offset)` — move the block to
    /// `height(x, z) + offset + localY`. Carried by every `terrain_matching` pool
    /// element (`StructureTemplatePool.Projection.TERRAIN_MATCHING`), which is what
    /// makes a village street follow a hillside.
    Gravity {
        /// The precomputed surface, see [`ColumnHeights`].
        heights: Arc<ColumnHeights>,
        /// `offset` — `-1` for the projection's own processor.
        offset: i32,
    },
    /// `CappedProcessor(delegate, limit)` — apply `delegate` to at most `limit`
    /// blocks of the piece, chosen by a shuffled walk over **every** index of the
    /// processed list.
    ///
    /// The one processor here that is not a per-block function, and the only one
    /// whose `evaluatesEntirePieceState` is true. Three properties are the
    /// specification:
    ///
    /// * the walk is over the *whole* piece, so it must not be clipped to a chunk
    ///   first — see [`super::template::StructureTemplate::place`];
    /// * `limit` is an `IntProvider` sampled **before** the shuffle. Every bundled
    ///   use is a bare int (`ConstantInt`), which draws nothing; a provider that
    ///   did draw would shift the whole shuffle, so
    ///   [`super::pool::PoolStore`] refuses anything else rather than assuming;
    /// * a delegate that returns the block unchanged consumes an index and **not**
    ///   the cap.
    Capped {
        /// The delegate, applied per selected index.
        delegate: Box<Processor>,
        /// `limit`, as the constant it always is in the bundled data.
        limit: i32,
    },
    /// `BlockAgeProcessor(mossiness)` — ruined portals' decay: stone/stone-bricks
    /// crack or moss over, stairs/slabs/walls moss, obsidian occasionally chills
    /// to crying obsidian. Five independent `state.is(...)` arms, each its own
    /// roll; see [`Self::process`] for the per-arm probabilities, transcribed from
    /// `BlockAgeProcessor.processBlock`.
    BlockAge {
        /// `mossiness` — the structure `Setup`'s own field, not a constant.
        mossiness: f32,
    },
    /// `LavaSubmergedBlockProcessor` — a block placed where the *pre-structure*
    /// world already held lava, and whose own shape is not a full cube (a stair,
    /// a slab, a wall run), is put back to full lava instead of leaving a
    /// part-block poking out of a lava pool.
    LavaSubmerged,
    /// `BlackstoneReplaceProcessor.INSTANCE` — the nether-side ruined-portal
    /// variant's stone-to-blackstone swap. Unconditional per block (no roll), and
    /// carries `facing`/`half`/`type` across the replacement when the source had
    /// them. Bundled with the overworld sets too (`replace_with_blackstone`
    /// exists as a `Setup` field) even though none of the six overworld ids sets
    /// it `true` today — `ruined_portal_nether` is the one that does, and it is
    /// out of scope (see the structure's module doc), so this arm is untested
    /// against a live placement and is exercised only by its own unit tests.
    BlackstoneReplace,
}

impl Processor {
    /// `processBlock` — the block to place, or `None` to drop it.
    #[must_use]
    pub fn process(&self, ctx: &ProcessCtx<'_>, block: ProcessedBlock) -> Option<ProcessedBlock> {
        match self {
            Self::BlockIgnore(ignored) => {
                if ignored.iter().any(|name| *name == block.state.name) {
                    None
                } else {
                    Some(block)
                }
            }
            Self::BlockRot { rottable, integrity } => {
                let applies = rottable
                    .as_ref()
                    .is_none_or(|set| set.contains(&block.state.name));
                if !applies {
                    return Some(block);
                }
                let mut random = LegacyRandomSource::new(get_seed(block.pos[0], block.pos[1], block.pos[2]));
                if random.next_float() <= *integrity {
                    Some(block)
                } else {
                    None
                }
            }
            Self::Rule(rules) => {
                let mut random = LegacyRandomSource::new(get_seed(block.pos[0], block.pos[1], block.pos[2]));
                for rule in rules {
                    if rule.input.test(&block.state, &mut random)
                        && rule.location.test_world(ctx.world, block.pos, &mut random)
                        && rule
                            .position
                            .test(ctx.local, block.pos, ctx.reference, &mut random)
                    {
                        return Some(ProcessedBlock {
                            pos: block.pos,
                            state: rule.output.clone(),
                        });
                    }
                }
                Some(block)
            }
            Self::ProtectedBlocks(cannot_replace) => {
                let existing = ctx.world.block_at(block.pos[0], block.pos[1], block.pos[2]);
                let name = existing.split_once('[').map_or(existing, |(n, _)| n);
                if cannot_replace.contains(name) {
                    None
                } else {
                    Some(block)
                }
            }
            Self::JigsawReplacement => {
                if block.state.name != "minecraft:jigsaw" {
                    return Some(block);
                }
                // Vanilla logs and keeps the jigsaw block when the nbt is
                // missing; keeping it is strictly more visible than dropping it,
                // so it is transcribed rather than "improved".
                let spec = ctx.nbt.and_then(|nbt| nbt_string(nbt, "final_state"));
                let spec = spec.unwrap_or("minecraft:air");
                let state = BlockState::parse(spec);
                if state.name == "minecraft:structure_void" {
                    None
                } else {
                    Some(ProcessedBlock {
                        pos: block.pos,
                        state,
                    })
                }
            }
            Self::Gravity { heights, offset } => {
                let height = heights.get(block.pos[0], block.pos[2]) + offset;
                Some(ProcessedBlock {
                    pos: [block.pos[0], height + ctx.local[1], block.pos[2]],
                    state: block.state,
                })
            }
            // `CappedProcessor.processBlock` is `StructureProcessor`'s default —
            // the identity. All of its work is in `finalizeProcessing`, which is
            // [`Self::finalize`].
            Self::Capped { .. } => Some(block),
            Self::BlockAge { mossiness } => {
                let mut random = LegacyRandomSource::new(get_seed(block.pos[0], block.pos[1], block.pos[2]));
                let name = block.state.name.as_str();
                let new_state = if matches!(
                    name,
                    "minecraft:stone_bricks" | "minecraft:stone" | "minecraft:chiseled_stone_bricks"
                ) {
                    block_age_full_stone(&mut random, *mossiness)
                } else if is_stairs(name) {
                    block_age_stairs(&block.state, &mut random, *mossiness)
                } else if is_slab(name) {
                    (random.next_float() < *mossiness).then(|| block_age_mossy(&block.state, "minecraft:mossy_stone_brick_slab"))
                } else if is_wall(name) {
                    (random.next_float() < *mossiness).then(|| block_age_mossy(&block.state, "minecraft:mossy_stone_brick_wall"))
                } else if name == "minecraft:obsidian" {
                    (random.next_float() < 0.15).then(|| BlockState::of("minecraft:crying_obsidian"))
                } else {
                    None
                };
                Some(match new_state {
                    Some(state) => ProcessedBlock { pos: block.pos, state },
                    None => block,
                })
            }
            Self::LavaSubmerged => {
                let existing = ctx.world.block_at(block.pos[0], block.pos[1], block.pos[2]);
                let was_lava = existing.split_once('[').map_or(existing, |(n, _)| n) == "minecraft:lava";
                if was_lava && !is_probably_full_cube(&block.state.name) {
                    Some(ProcessedBlock {
                        pos: block.pos,
                        state: BlockState::of("minecraft:lava"),
                    })
                } else {
                    Some(block)
                }
            }
            Self::BlackstoneReplace => {
                let Some(replacement) = blackstone_replacement(&block.state.name) else {
                    return Some(block);
                };
                let mut state = BlockState::of(replacement);
                for key in ["facing", "half", "type"] {
                    if let Some(value) = block.state.properties.get(key) {
                        state.properties.insert(key.to_string(), value.clone());
                    }
                }
                Some(ProcessedBlock { pos: block.pos, state })
            }
        }
    }

    /// `evaluatesEntirePieceState()` — true only for [`Self::Capped`].
    ///
    /// The flag vanilla's `processBlockInfos` reads to decide whether to build the
    /// block list for the whole piece or only for the decorating chunk. It is not a
    /// performance switch: the capped walk indexes the list, so a shorter list is a
    /// different structure.
    #[must_use]
    pub fn evaluates_entire_piece_state(&self) -> bool {
        matches!(self, Self::Capped { .. })
    }

    /// `finalizeProcessing(level, position, referencePos, original, processed,
    /// settings)` — a no-op for every processor except [`Self::Capped`].
    ///
    /// Rewrites `processed` in place. `originals` carries each surviving block's
    /// template-local position and `nbt`, which is what the delegate's
    /// `templateRelativePos` argument is (vanilla passes `originalBlockInfo.pos()`,
    /// and the *original* list holds template-local positions while the processed
    /// list holds world ones — reading the wrong one is a silent divergence for
    /// every position-sensitive delegate).
    pub fn finalize(
        &self,
        position: [i32; 3],
        reference: [i32; 3],
        seed: i64,
        originals: &[([i32; 3], Option<Arc<BlockNbt>>)],
        processed: &mut [ProcessedBlock],
        world: &dyn WorldRead,
    ) {
        let Self::Capped { delegate, limit } = self else {
            return;
        };
        // `this.limit.maxInclusive() != 0 && !processedBlockInfoList.isEmpty()`.
        if *limit == 0 || processed.is_empty() || originals.len() != processed.len() {
            return;
        }
        // `RandomSource.createThreadLocalInstance(level.getSeed()).forkPositional().at(position)`.
        // `SingleThreadedRandomSource` is bit-identical to `LegacyRandomSource` —
        // same LCG, same `next(bits)` — and it forks into the *same*
        // `LegacyPositionalRandomFactory`, so the derivation is exactly this.
        let mut random = LegacyRandomSource::new(seed)
            .fork_positional()
            .at(position[0], position[1], position[2]);
        // `ConstantInt.sample` draws nothing; see the variant doc for why nothing
        // else is accepted.
        let max_to_replace = (*limit).min(i32::try_from(processed.len()).unwrap_or(i32::MAX));
        if max_to_replace < 1 {
            return;
        }
        let indices = shuffled_indices(processed.len(), &mut random);
        let mut replaced = 0;
        for index in indices {
            if replaced >= max_to_replace {
                break;
            }
            let (local, nbt) = &originals[index];
            let ctx = ProcessCtx {
                local: *local,
                reference,
                nbt: nbt.as_deref(),
                world,
            };
            let before = processed[index].clone();
            if let Some(altered) = delegate.process(&ctx, before.clone()) {
                if altered != before {
                    replaced += 1;
                    processed[index] = altered;
                }
            }
        }
    }

    /// `BlockIgnoreProcessor.STRUCTURE_AND_AIR`.
    #[must_use]
    pub fn structure_and_air() -> Self {
        Self::BlockIgnore(vec![
            "minecraft:air".to_string(),
            "minecraft:structure_block".to_string(),
        ])
    }

    /// `BlockIgnoreProcessor.STRUCTURE_BLOCK`.
    #[must_use]
    pub fn structure_block() -> Self {
        Self::BlockIgnore(vec!["minecraft:structure_block".to_string()])
    }
}

/// `BlockAgeProcessor.maybeReplaceFullStoneBlock` — stone/stone-bricks/chiseled
/// crack or moss. **Both** candidate arrays are built unconditionally before the
/// mossiness roll picks one, exactly as `NON_MOSSY_REPLACEMENTS`/
/// `MOSSY_REPLACEMENTS`'s local-array construction does in Java (the
/// non-mossy branch's own `getRandomFacingStairs` call still draws two values
/// even when the mossy branch is the one that ends up chosen) — so the draw
/// order here is `[full-block roll] [non-mossy stairs facing] [non-mossy stairs
/// half] [mossy stairs facing] [mossy stairs half] [mossiness roll] [array
/// index]`, seven draws deep on the branch that does not bail out at the first
/// roll.
fn block_age_full_stone(random: &mut LegacyRandomSource, mossiness: f32) -> Option<BlockState> {
    if random.next_float() >= 0.5 {
        return None;
    }
    let non_mossy = [
        BlockState::of("minecraft:cracked_stone_bricks"),
        random_facing_stairs(random, "minecraft:stone_brick_stairs"),
    ];
    let mossy = [
        BlockState::of("minecraft:mossy_stone_bricks"),
        random_facing_stairs(random, "minecraft:mossy_stone_brick_stairs"),
    ];
    Some(pick_mossy_or_not(random, mossiness, &non_mossy, &mossy))
}

/// `BlockAgeProcessor.maybeReplaceStairs` — a stairs block either stays, or
/// becomes one of two non-mossy slabs or a mossy stairs/slab pair. The mossy
/// stairs replacement copies `blockState`'s own properties
/// (`withPropertiesOf`) rather than drawing a fresh facing — stairs and
/// mossy-stairs are the same block class, so every property the source has a
/// value for exists on the target too.
fn block_age_stairs(state: &BlockState, random: &mut LegacyRandomSource, mossiness: f32) -> Option<BlockState> {
    if random.next_float() >= 0.5 {
        return None;
    }
    let mut mossy_stairs = state.clone();
    mossy_stairs.name = "minecraft:mossy_stone_brick_stairs".to_string();
    let mossy = [mossy_stairs, BlockState::of("minecraft:mossy_stone_brick_slab")];
    let non_mossy = [
        BlockState::of("minecraft:stone_slab"),
        BlockState::of("minecraft:stone_brick_slab"),
    ];
    Some(pick_mossy_or_not(random, mossiness, &non_mossy, &mossy))
}

/// `BlockAgeProcessor.maybeReplaceSlab`/`maybeReplaceWall` — one roll, `state`'s
/// own properties carried onto the mossy block of the same class.
fn block_age_mossy(state: &BlockState, new_name: &str) -> BlockState {
    let mut moss = state.clone();
    moss.name = new_name.to_string();
    moss
}

/// Vanilla's own random-facing-stairs helper — its own horizontal-plane's
/// own face order is `[NORTH, EAST, SOUTH, WEST]`
/// (vanilla's own horizontal-plane array literal, not the direction's
/// `2D data value` order `coded::Facing` uses), then vanilla's own half-enum
/// values are
/// `[TOP, BOTTOM]`.
fn random_facing_stairs(random: &mut LegacyRandomSource, name: &str) -> BlockState {
    const FACES: [&str; 4] = ["north", "east", "south", "west"];
    const HALVES: [&str; 2] = ["top", "bottom"];
    let facing = FACES[random.next_int_bounded(4).clamp(0, 3) as usize];
    let half = HALVES[random.next_int_bounded(2).clamp(0, 1) as usize];
    BlockState::parse(&format!("{name}[facing={facing},half={half}]"))
}

/// `getRandomBlock(random, nonMossyBlocks, mossyBlocks)` — one `nextFloat()`
/// against `mossiness`, then one `nextInt(2)` over whichever two-element array
/// was picked.
fn pick_mossy_or_not(
    random: &mut LegacyRandomSource,
    mossiness: f32,
    non_mossy: &[BlockState; 2],
    mossy: &[BlockState; 2],
) -> BlockState {
    let chosen = if random.next_float() < mossiness { mossy } else { non_mossy };
    let index = random.next_int_bounded(2).clamp(0, 1) as usize;
    chosen[index].clone()
}

/// `state.is(BlockTags.STAIRS)` / `SLABS` / `WALLS`, approximated by naming
/// convention rather than a real tag table (this crate has none) — exact for
/// every vanilla stairs/slab/wall id, which is the whole domain
/// [`Processor::BlockAge`] is ever handed (a ruined-portal template's palette).
fn is_stairs(name: &str) -> bool {
    name.ends_with("_stairs")
}
fn is_slab(name: &str) -> bool {
    name.ends_with("_slab")
}
fn is_wall(name: &str) -> bool {
    name.ends_with("_wall")
}

/// `Block.isShapeFullBlock(state.getShape(...))`, approximated with a denylist
/// of block-id substrings that are never a full cube — this crate has no
/// collision-shape table (that lives in `lodestone-data`, which worldgen does
/// not depend on). Exact for every full-cube id [`Processor::LavaSubmerged`]
/// can be handed by the processor chain ahead of it (stone/stone-bricks
/// variants, obsidian, gold block, netherrack, magma block) and approximate
/// only at the handful of partial shapes a `RuleProcessor`/[`Processor::BlockAge`]
/// upstream of this one can introduce (stairs, slabs, walls, bars).
fn is_probably_full_cube(name: &str) -> bool {
    const NOT_FULL: &[&str] = &[
        "stairs", "slab", "wall", "fence", "gate", "trapdoor", "door", "pane", "bars", "carpet",
        "pressure_plate", "button", "torch", "ladder", "vine", "chain", "lantern", "campfire",
        "rail", "anvil", "lily_pad", "repeater", "comparator", "rod", "bed", "banner", "sign",
        "flower", "mushroom", "coral", "sapling", "bush", "leaves", "snow_layer", "candle",
        "cake", "web", "air", "chest", "barrel", "scaffolding", "lever", "grindstone",
        "cauldron", "bell", "conduit", "flower_pot", "skull", "head",
    ];
    !NOT_FULL.iter().any(|s| name.contains(s))
}

/// `BlackstoneReplaceProcessor`'s replacement map — a stone-family block id to
/// its blackstone counterpart. `None` for anything not in vanilla's table,
/// which the processor keeps unchanged.
fn blackstone_replacement(name: &str) -> Option<&'static str> {
    Some(match name {
        "minecraft:cobblestone" | "minecraft:mossy_cobblestone" => "minecraft:blackstone",
        "minecraft:stone" => "minecraft:polished_blackstone",
        "minecraft:stone_bricks" | "minecraft:mossy_stone_bricks" => "minecraft:polished_blackstone_bricks",
        "minecraft:cobblestone_stairs" | "minecraft:mossy_cobblestone_stairs" => "minecraft:blackstone_stairs",
        "minecraft:stone_stairs" => "minecraft:polished_blackstone_stairs",
        "minecraft:stone_brick_stairs" | "minecraft:mossy_stone_brick_stairs" => {
            "minecraft:polished_blackstone_brick_stairs"
        }
        "minecraft:cobblestone_slab" | "minecraft:mossy_cobblestone_slab" => "minecraft:blackstone_slab",
        "minecraft:smooth_stone_slab" | "minecraft:stone_slab" => "minecraft:polished_blackstone_slab",
        "minecraft:stone_brick_slab" | "minecraft:mossy_stone_brick_slab" => {
            "minecraft:polished_blackstone_brick_slab"
        }
        "minecraft:stone_brick_wall" | "minecraft:mossy_stone_brick_wall" => "minecraft:polished_blackstone_brick_wall",
        "minecraft:cobblestone_wall" | "minecraft:mossy_cobblestone_wall" => "minecraft:blackstone_wall",
        "minecraft:chiseled_stone_bricks" => "minecraft:chiseled_polished_blackstone",
        "minecraft:cracked_stone_bricks" => "minecraft:cracked_polished_blackstone_bricks",
        "minecraft:iron_bars" => "minecraft:iron_chain",
        _ => return None,
    })
}

/// `Util.toShuffledList(IntStream.range(0, n), random)` — the **int** overload,
/// whose swap is `result.set(i - 1, result.set(swapTo, result.getInt(i - 1)))`.
///
/// That nested `set` is a swap, so this is the same downward Fisher–Yates as
/// [`super::pool::shuffle`] and costs `max(0, n - 1)` draws. Written out rather
/// than reusing that function because the two are separate methods in vanilla and
/// a future change to either must not silently move the other.
fn shuffled_indices(n: usize, random: &mut LegacyRandomSource) -> Vec<usize> {
    let mut out: Vec<usize> = (0..n).collect();
    let mut i = i32::try_from(n).unwrap_or(i32::MAX);
    while i > 1 {
        let swap_to = random.next_int_bounded(i);
        out.swap((i - 1) as usize, swap_to.clamp(0, i - 1) as usize);
        i -= 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A world that is air everywhere — the shape most processor tests want.
    struct Air;
    impl WorldRead for Air {
        fn block_at(&self, _x: i32, _y: i32, _z: i32) -> &str {
            "minecraft:air"
        }
    }

    fn ctx<'a>(world: &'a dyn WorldRead, nbt: Option<&'a BlockNbt>) -> ProcessCtx<'a> {
        ProcessCtx {
            local: [0, 0, 0],
            reference: [0, 0, 0],
            nbt,
            world,
        }
    }

    fn at(pos: [i32; 3], name: &str) -> ProcessedBlock {
        ProcessedBlock {
            pos,
            state: BlockState::of(name),
        }
    }

    #[test]
    fn block_ignore_drops_air_and_keeps_everything_else() {
        let processor = Processor::structure_and_air();
        let world = Air;
        let ctx = ctx(&world, None);
        assert!(processor.process(&ctx, at([0, 0, 0], "minecraft:air")).is_none());
        assert!(
            processor
                .process(&ctx, at([0, 0, 0], "minecraft:structure_block"))
                .is_none()
        );
        assert!(
            processor
                .process(&ctx, at([0, 0, 0], "minecraft:spruce_planks"))
                .is_some()
        );
    }

    /// Integrity 1.0 keeps everything, 0.0 keeps nothing, and the answer for a
    /// given position does not depend on when it is asked — the property that
    /// lets two chunks place two halves of one piece.
    #[test]
    fn block_rot_is_position_deterministic_and_respects_the_extremes() {
        let keep_all = Processor::BlockRot {
            rottable: None,
            integrity: 1.0,
        };
        let drop_all = Processor::BlockRot {
            rottable: None,
            integrity: 0.0,
        };
        let world = Air;
        let ctx = ctx(&world, None);
        let mut kept = 0;
        for x in 0..64 {
            let pos = [x, 62, -13];
            assert!(keep_all.process(&ctx, at(pos, "minecraft:stone_bricks")).is_some());
            assert!(drop_all.process(&ctx, at(pos, "minecraft:stone_bricks")).is_none());
            let half = Processor::BlockRot {
                rottable: None,
                integrity: 0.5,
            };
            let first = half.process(&ctx, at(pos, "minecraft:stone_bricks")).is_some();
            assert_eq!(first, half.process(&ctx, at(pos, "minecraft:stone_bricks")).is_some());
            kept += usize::from(first);
        }
        // 0.5 integrity over 64 positions must be a mixture, not all-or-nothing.
        assert!((8..56).contains(&kept), "integrity 0.5 kept {kept}/64");
    }

    /// `rottable_blocks` present means "only these rot"; `None` means "everything
    /// rots". An empty set is the third, distinct case and must rot nothing.
    #[test]
    fn rottable_blocks_narrows_rather_than_disables() {
        let world = Air;
        let ctx = ctx(&world, None);
        let only_planks = Processor::BlockRot {
            rottable: Some(Arc::new(
                ["minecraft:oak_planks".to_string()].into_iter().collect(),
            )),
            integrity: 0.0,
        };
        assert!(only_planks.process(&ctx, at([1, 2, 3], "minecraft:oak_planks")).is_none());
        assert!(only_planks.process(&ctx, at([1, 2, 3], "minecraft:cobblestone")).is_some());
        let nothing_rots = Processor::BlockRot {
            rottable: Some(Arc::new(HashSet::new())),
            integrity: 0.0,
        };
        assert!(nothing_rots.process(&ctx, at([1, 2, 3], "minecraft:oak_planks")).is_some());
    }

    /// A jigsaw block becomes its own `final_state`, and a `structure_void`
    /// `final_state` drops it.
    #[test]
    fn jigsaw_replacement_reads_final_state_from_the_retained_nbt() {
        let world = Air;
        let planks: BlockNbt = vec![(
            "final_state".to_string(),
            lodestone_core::Nbt::String("minecraft:oak_planks".into()),
        )];
        let void: BlockNbt = vec![(
            "final_state".to_string(),
            lodestone_core::Nbt::String("minecraft:structure_void".into()),
        )];
        let jigsaw = at([4, 70, 9], "minecraft:jigsaw");
        let replaced = Processor::JigsawReplacement
            .process(&ctx(&world, Some(&planks)), jigsaw.clone())
            .expect("a planks final_state is placed");
        assert_eq!(replaced.state.name, "minecraft:oak_planks");
        assert_eq!(replaced.pos, [4, 70, 9]);
        assert!(
            Processor::JigsawReplacement
                .process(&ctx(&world, Some(&void)), jigsaw.clone())
                .is_none()
        );
        // A non-jigsaw block is untouched even with the nbt present.
        assert_eq!(
            Processor::JigsawReplacement
                .process(&ctx(&world, Some(&planks)), at([4, 70, 9], "minecraft:cobblestone"))
                .expect("kept")
                .state
                .name,
            "minecraft:cobblestone"
        );
    }

    /// `location_predicate` reads the **world**, not the template — the village
    /// street bridge rule. A `dirt_path` over water becomes planks; the same
    /// `dirt_path` over grass does not.
    #[test]
    fn a_location_predicate_reads_the_world_state() {
        struct Water;
        impl WorldRead for Water {
            fn block_at(&self, _x: i32, _y: i32, _z: i32) -> &str {
                "minecraft:water[level=0]"
            }
        }
        let rules = vec![ProcessorRule {
            input: RuleTest::BlockMatch("minecraft:dirt_path".into()),
            location: RuleTest::BlockMatch("minecraft:water".into()),
            position: PosTest::AlwaysTrue,
            output: BlockState::of("minecraft:oak_planks"),
        }];
        let processor = Processor::Rule(rules);
        let over_water = Water;
        assert_eq!(
            processor
                .process(&ctx(&over_water, None), at([0, 63, 0], "minecraft:dirt_path"))
                .expect("kept")
                .state
                .name,
            "minecraft:oak_planks"
        );
        let over_air = Air;
        assert_eq!(
            processor
                .process(&ctx(&over_air, None), at([0, 63, 0], "minecraft:dirt_path"))
                .expect("kept")
                .state
                .name,
            "minecraft:dirt_path"
        );
    }

    /// Gravity moves a block to `surface + offset + localY`, and its answer is a
    /// pure function of the precomputed table — so the same piece placed from two
    /// chunks agrees.
    #[test]
    fn gravity_lands_a_local_zero_block_on_the_surface() {
        let heights = Arc::new(ColumnHeights::build(0, 0, 3, 3, |x, z| 64 + x + z));
        let processor = Processor::Gravity {
            heights: Arc::clone(&heights),
            offset: -1,
        };
        let world = Air;
        for (local_y, expected) in [(0, 64 + 2 + 3 - 1), (5, 64 + 2 + 3 - 1 + 5)] {
            let moved = processor
                .process(
                    &ProcessCtx {
                        local: [0, local_y, 0],
                        reference: [0, 0, 0],
                        nbt: None,
                        world: &world,
                    },
                    at([2, 90, 3], "minecraft:dirt_path"),
                )
                .expect("gravity never drops a block");
            assert_eq!(moved.pos, [2, expected, 3], "local y {local_y}");
        }
        // Outside the table the edge value is reused rather than panicking.
        assert_eq!(heights.get(-9, -9), 64);
        assert_eq!(heights.get(99, 99), 64 + 3 + 3);
    }

    /// `high_rampart`'s own numbers, with the chance **predicted** at four
    /// distances from `clampedLerp(inverseLerp(d, 0, 100), 0.0, 0.05)` rather than
    /// asserted to "increase with height".
    ///
    /// The wrong-argument-order hypothesis is excluded explicitly: reading
    /// `clampedLerp` as `lerp(start, end, t)` gives a **constant** `0.0` at every
    /// distance, so the d=100 row alone falsifies it.
    #[test]
    fn axis_aligned_linear_pos_ramps_the_chance_along_one_axis() {
        let test = PosTest::AxisAlignedLinear {
            min_chance: 0.0,
            max_chance: 0.05,
            min_dist: 0,
            max_dist: 100,
            axis: Axis::Y,
        };
        // The chance at each distance, recovered by counting acceptances over many
        // independent streams — each `test` call is exactly one `nextFloat()`, so
        // the acceptance rate *is* the chance.
        //
        // One long stream rather than one fresh `LegacyRandomSource` per trial:
        // sequentially seeded LCGs are strongly correlated in their first draw, so
        // the per-seed spelling of this measurement is biased by several σ and
        // would have to be given a tolerance loose enough to pass under a wrong
        // chance too.
        for (dy, expected) in [(0, 0.0_f32), (25, 0.0125), (50, 0.025), (200, 0.05)] {
            let mut accepted = 0;
            let trials = 200_000;
            let mut random = LegacyRandomSource::new(0xBEEF_1234);
            for _ in 0..trials {
                if test.test([0, 0, 0], [0, dy, 0], [0, 0, 0], &mut random) {
                    accepted += 1;
                }
            }
            #[allow(clippy::cast_precision_loss)]
            let rate = f64::from(accepted) / f64::from(trials);
            let expected = f64::from(expected);
            assert!(
                (rate - expected).abs() < 0.002,
                "dy {dy}: measured {rate}, predicted {expected}"
            );
        }
        // The axis is honoured: an X offset with an axis-Y test is distance 0.
        let mut a = LegacyRandomSource::new(5);
        let mut b = LegacyRandomSource::new(5);
        assert_eq!(
            test.test([0, 0, 0], [999, 0, 0], [0, 0, 0], &mut a),
            test.test([0, 0, 0], [0, 0, 0], [0, 0, 0], &mut b),
            "an X displacement must not move an axis-Y test"
        );
        // Exactly one draw, and `always_true` costs none.
        let mut drawn = LegacyRandomSource::new(11);
        let _ = test.test([0, 0, 0], [0, 7, 0], [0, 0, 0], &mut drawn);
        let mut oracle = LegacyRandomSource::new(11);
        let _ = oracle.next_float();
        assert_eq!(drawn.next_int_bounded(1_000_000), oracle.next_int_bounded(1_000_000));
        let mut untouched = LegacyRandomSource::new(11);
        let mut control = LegacyRandomSource::new(11);
        assert!(PosTest::AlwaysTrue.test([0, 0, 0], [0, 7, 0], [0, 0, 0], &mut untouched));
        assert_eq!(
            untouched.next_int_bounded(1_000_000),
            control.next_int_bounded(1_000_000),
            "always_true consumed a draw"
        );
    }

    /// The cap is exact, not approximate: a delegate that would convert every block
    /// converts **exactly `limit`** of them, and the same piece converts the same
    /// indices every time.
    ///
    /// The magnitude hypothesis this excludes: "capped replaces some blocks". A
    /// `limit` of 6 over 40 candidates must be 6 — 40 (cap ignored) and 0 (finalize
    /// never runs) are both plausible bugs that produce a visually fine ruin.
    #[test]
    fn capped_replaces_exactly_the_limit_and_is_position_stable() {
        let delegate = Processor::Rule(vec![ProcessorRule {
            input: RuleTest::BlockMatch("minecraft:gravel".into()),
            location: RuleTest::AlwaysTrue,
            position: PosTest::AlwaysTrue,
            output: BlockState::of("minecraft:suspicious_gravel"),
        }]);
        let capped = Processor::Capped {
            delegate: Box::new(delegate),
            limit: 6,
        };
        let world = Air;
        let build = || {
            (0..40)
                .map(|i| ProcessedBlock {
                    pos: [i, 64, 3],
                    state: BlockState::of("minecraft:gravel"),
                })
                .collect::<Vec<_>>()
        };
        let originals: Vec<([i32; 3], Option<Arc<BlockNbt>>)> =
            (0..40).map(|i| ([i, 0, 0], None)).collect();

        let mut processed = build();
        capped.finalize([16, 64, 0], [20, 60, 4], -195_764_831, &originals, &mut processed, &world);
        let converted: Vec<i32> = processed
            .iter()
            .filter(|b| b.state.name == "minecraft:suspicious_gravel")
            .map(|b| b.pos[0])
            .collect();
        assert_eq!(converted.len(), 6, "converted {converted:?}");

        // Same piece position → same indices. This is what lets two chunks place
        // two halves of one trail-ruins house and agree.
        let mut again = build();
        capped.finalize([16, 64, 0], [20, 60, 4], -195_764_831, &originals, &mut again, &world);
        assert_eq!(processed, again);

        // A different piece position picks a different set, or the positional fork
        // is not being used at all.
        let mut elsewhere = build();
        capped.finalize([48, 64, 0], [20, 60, 4], -195_764_831, &originals, &mut elsewhere, &world);
        assert_ne!(processed, elsewhere);

        // A delegate that changes nothing consumes indices and not the cap, so
        // nothing is converted and nothing panics.
        let inert = Processor::Capped {
            delegate: Box::new(Processor::Rule(vec![ProcessorRule {
                input: RuleTest::BlockMatch("minecraft:cobblestone".into()),
                location: RuleTest::AlwaysTrue,
                position: PosTest::AlwaysTrue,
                output: BlockState::of("minecraft:suspicious_gravel"),
            }])),
            limit: 6,
        };
        let mut untouched = build();
        let before = untouched.clone();
        inert.finalize([16, 64, 0], [20, 60, 4], -195_764_831, &originals, &mut untouched, &world);
        assert_eq!(untouched, before);

        // A `limit` of 0 is vanilla's own early return.
        let zero = Processor::Capped {
            delegate: Box::new(Processor::Rule(vec![ProcessorRule {
                input: RuleTest::AlwaysTrue,
                location: RuleTest::AlwaysTrue,
                position: PosTest::AlwaysTrue,
                output: BlockState::of("minecraft:suspicious_gravel"),
            }])),
            limit: 0,
        };
        let mut none = build();
        zero.finalize([16, 64, 0], [20, 60, 4], -195_764_831, &originals, &mut none, &world);
        assert_eq!(none, before);
    }

    /// `BlockAgeProcessor` at `mossiness=0.0` never produces a mossy variant —
    /// the discriminating end of the probability range, not a plausible middle
    /// value. Also the negative-space case: `minecraft:diorite` matches none of
    /// the five `state.is(...)` arms and must pass through untouched.
    #[test]
    fn block_age_at_zero_mossiness_never_moss_and_leaves_other_blocks_alone() {
        let processor = Processor::BlockAge { mossiness: 0.0 };
        let world = Air;
        let ctx = ctx(&world, None);
        let mut saw_a_change = false;
        for i in 0..200 {
            let out = processor
                .process(&ctx, at([i, 70, 3], "minecraft:stone_bricks"))
                .expect("kept");
            assert!(
                !out.state.name.contains("mossy"),
                "mossiness 0.0 produced {}",
                out.state.name
            );
            saw_a_change |= out.state.name != "minecraft:stone_bricks";
        }
        // Some positions still roll below the 0.5 "leave it alone" gate and
        // become cracked/slab variants — mossiness 0.0 only forbids *moss*.
        assert!(saw_a_change, "no position rolled a non-mossy decay variant");
        assert_eq!(
            processor
                .process(&ctx, at([0, 70, 0], "minecraft:diorite"))
                .expect("kept")
                .state
                .name,
            "minecraft:diorite"
        );
    }

    /// At `mossiness=1.0` every full-stone-block replacement that fires is
    /// mossy, and the position-seeded stream is reproducible.
    #[test]
    fn block_age_at_full_mossiness_only_ever_mosses() {
        let processor = Processor::BlockAge { mossiness: 1.0 };
        let world = Air;
        let ctx = ctx(&world, None);
        let mut replaced = 0;
        for i in 0..200 {
            let pos = [i, 64, -7];
            let first = processor.process(&ctx, at(pos, "minecraft:stone")).expect("kept");
            let again = processor.process(&ctx, at(pos, "minecraft:stone")).expect("kept");
            assert_eq!(first, again, "position {i} is not reproducible");
            if first.state.name != "minecraft:stone" {
                replaced += 1;
                assert!(
                    first.state.name == "minecraft:mossy_stone_bricks"
                        || first.state.name.starts_with("minecraft:mossy_stone_brick_stairs"),
                    "mossiness 1.0 produced a non-mossy replacement: {}",
                    first.state.name
                );
            }
        }
        assert!((60..140).contains(&replaced), "replaced {replaced}/200 at the ~50% gate");
    }

    /// A stairs block's mossy replacement keeps the source's own `facing`
    /// (`withPropertiesOf`), rather than drawing a fresh one.
    #[test]
    fn block_age_stairs_carries_its_own_facing_into_the_mossy_variant() {
        let processor = Processor::BlockAge { mossiness: 1.0 };
        let world = Air;
        let ctx = ctx(&world, None);
        for facing in ["north", "east", "south", "west"] {
            let source = BlockState::parse(&format!("minecraft:stone_brick_stairs[facing={facing},half=bottom]"));
            for i in 0..50 {
                let out = processor
                    .process(
                        &ctx,
                        ProcessedBlock {
                            pos: [i, 80, 12],
                            state: source.clone(),
                        },
                    )
                    .expect("kept");
                if out.state.name == "minecraft:mossy_stone_brick_stairs" {
                    assert_eq!(out.state.properties.get("facing").map(String::as_str), Some(facing));
                    assert_eq!(out.state.properties.get("half").map(String::as_str), Some("bottom"));
                }
            }
        }
    }

    /// `LavaSubmergedBlockProcessor`: a non-full shape placed where the
    /// pre-structure world held lava goes back to lava; a full block, or a
    /// non-lava world, is untouched.
    #[test]
    fn lava_submerged_reclaims_only_non_full_shapes_over_lava() {
        struct Lava;
        impl WorldRead for Lava {
            fn block_at(&self, _x: i32, _y: i32, _z: i32) -> &str {
                "minecraft:lava[level=0]"
            }
        }
        let processor = Processor::LavaSubmerged;
        let over_lava = Lava;
        let stairs = processor
            .process(&ctx(&over_lava, None), at([0, 60, 0], "minecraft:stone_brick_stairs"))
            .expect("kept");
        assert_eq!(stairs.state.name, "minecraft:lava");
        let full_block = processor
            .process(&ctx(&over_lava, None), at([0, 60, 0], "minecraft:obsidian"))
            .expect("kept");
        assert_eq!(full_block.state.name, "minecraft:obsidian");
        let over_air = Air;
        let stairs_over_air = processor
            .process(&ctx(&over_air, None), at([0, 60, 0], "minecraft:stone_brick_stairs"))
            .expect("kept");
        assert_eq!(stairs_over_air.state.name, "minecraft:stone_brick_stairs");
    }

    /// `BlackstoneReplaceProcessor`: named table entries swap and carry
    /// `facing`/`half`/`type`; anything else, including a block with no entry,
    /// passes through.
    #[test]
    fn blackstone_replace_swaps_the_table_and_carries_orientation() {
        let processor = Processor::BlackstoneReplace;
        let world = Air;
        let ctx = ctx(&world, None);
        let cobble = processor
            .process(&ctx, at([0, 0, 0], "minecraft:cobblestone"))
            .expect("kept");
        assert_eq!(cobble.state.name, "minecraft:blackstone");
        let stairs = processor
            .process(
                &ctx,
                ProcessedBlock {
                    pos: [0, 0, 0],
                    state: BlockState::parse("minecraft:stone_brick_stairs[facing=east,half=top]"),
                },
            )
            .expect("kept");
        assert_eq!(stairs.state.name, "minecraft:polished_blackstone_brick_stairs");
        assert_eq!(stairs.state.properties.get("facing").map(String::as_str), Some("east"));
        assert_eq!(stairs.state.properties.get("half").map(String::as_str), Some("top"));
        let untouched = processor
            .process(&ctx, at([0, 0, 0], "minecraft:oak_planks"))
            .expect("kept");
        assert_eq!(untouched.state.name, "minecraft:oak_planks");
    }

    /// `Util.toShuffledList`'s int overload is a permutation and costs `n - 1`
    /// draws, matched against a hand-expanded downward Fisher–Yates.
    #[test]
    fn shuffled_indices_is_the_downward_fisher_yates() {
        let mut oracle = LegacyRandomSource::new(3);
        let picks = [
            oracle.next_int_bounded(5),
            oracle.next_int_bounded(4),
            oracle.next_int_bounded(3),
            oracle.next_int_bounded(2),
        ];
        let mut expected: Vec<usize> = (0..5).collect();
        expected.swap(4, picks[0] as usize);
        expected.swap(3, picks[1] as usize);
        expected.swap(2, picks[2] as usize);
        expected.swap(1, picks[3] as usize);

        let mut random = LegacyRandomSource::new(3);
        assert_eq!(shuffled_indices(5, &mut random), expected);
        // Stream position: exactly four draws for five indices.
        assert_eq!(
            random.next_int_bounded(1_000_000),
            oracle.next_int_bounded(1_000_000)
        );
    }
}
