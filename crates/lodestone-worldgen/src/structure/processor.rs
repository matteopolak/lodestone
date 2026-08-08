//! Template **processors** — the per-block filters vanilla runs between a
//! template's palette and the world (issue #514's S2, widened by S4).
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
//! * `capped` and `block_entity_modifier` are deliberately absent and ledgered:
//!   the first needs a shuffled-index walk over the whole processed list plus the
//!   archaeology loot pass, the second needs block entities in worldgen.

use std::collections::HashSet;
use std::sync::Arc;

use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, get_seed};

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
#[derive(Debug, Clone)]
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

/// One `ProcessorRule`: match the template state (and optionally the world state
/// at the target), emit `output`.
///
/// `position_predicate` is **not** represented: the only bundled use is
/// `axis_aligned_linear_pos` in `high_rampart` (the bastion), and it is named on
/// the ledger rather than defaulted to true, because defaulting it to true would
/// change the *draw count* of every later rule in that list.
#[derive(Debug, Clone)]
pub struct ProcessorRule {
    /// `input_predicate`, tested against the template's state.
    pub input: RuleTest,
    /// `location_predicate`, tested against the world's state at the target.
    pub location: RuleTest,
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
}
