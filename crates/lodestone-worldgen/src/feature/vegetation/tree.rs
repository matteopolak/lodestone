//! Tree geometry: the trunk placers (straight, forking, dark-oak 2×2), the foliage
//! placers, and the `distance` leaf-state propagation that finishes a canopy.
//!
//! Moved here verbatim from `feature/vegetation.rs` by U16 Phase B.

use std::cell::RefCell;
use std::collections::VecDeque;

use lodestone_worldgen_core::hash::FastSet;

use serde_json::Value;

use crate::feature::{BlockPos, IntProvider};
use crate::rng::RandomSource;

use super::config::{BlockStateProvider, VegTags, try_parse_int_provider};
use super::grid::VegGrid;
use super::ids::{Rewrite, Tag};

/// Vanilla's own trunk-placer base class (the
/// `Straight`/`Forking` subset — the savanna/acacia increment adds `Forking`, acacia's real
/// trunk placer, alongside the `Straight` this module shipped with
/// originally). Both variants carry the identical `(base_height, height_rand_a,
/// height_rand_b)` triple `TrunkPlacer.getTreeHeight` (a base-class method,
/// not overridden by either subclass) draws from — kept as one shared shape
/// rather than duplicating the three fields per variant.
///
/// **No longer `Copy`** since the `Cherry`/`UpwardsBranching`
/// variants carry `IntProvider` fields (not `Copy` — `WeightedList`/`.. `
/// hold a `Vec`). Every match against this type already binds by reference
/// (`match self`/`match &cfg.trunk_placer`, never `match *self`), so nothing
/// downstream needed to change.
#[derive(Clone, Debug)]
pub enum TrunkPlacerCfg {
    Straight {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
    /// `ForkingTrunkPlacer` — acacia's real trunk: a single
    /// leaning column, plus (usually) one branch in a different horizontal
    /// direction. See [`place_trunk`] for the port of `placeTrunk` itself.
    Forking {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
    /// `DarkOakTrunkPlacer` — dark oak's real trunk: a 2×2 log
    /// column (four logs per level) that leans one step for its upper
    /// portion, on a 2×2 `below_trunk_provider` base, plus up to a few short
    /// hanging branches around the canopy top. See [`place_dark_oak_trunk`]
    /// for the port of `placeTrunk` itself. Shared with pale oak, which uses
    /// the same placer type with its own providers.
    DarkOak {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
    /// `GiantTrunkPlacer` — the redwood/giant-spruce trunk: a
    /// static 2×2 log column, no lean, no anchor gate. Also the base shape
    /// [`Self::MegaJungle`] places via its own `super.placeTrunk` before
    /// adding branches — see [`place_giant_trunk`] for the port.
    Giant {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
    /// `MegaJungleTrunkPlacer` — mega jungle's real trunk:
    /// [`Self::Giant`]'s own 2×2 column, then a spiral of short radial
    /// branches. See [`place_mega_jungle_trunk`] for the port.
    MegaJungle {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
    /// `FancyTrunkPlacer` — the `fancy_oak_*`/`fancy_oak_checked` branch
    /// shared by oak, jungle and dark_forest (this change's highest-value
    /// remaining gap). Structurally distinct from every other placer here:
    /// a slim central trunk plus a scattered spray of diagonal limbs, each
    /// walked out from a randomly-angled, randomly-scaled offset and only
    /// actually grown if a dry-run check finds room. See
    /// [`place_fancy_trunk`] for the port of `placeTrunk` itself.
    Fancy {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
    /// `CherryTrunkPlacer` — cherry's real trunk: a single
    /// straight column, then one or two (and sometimes a third, dead-centre)
    /// side branches climbing away from the trunk. See [`place_cherry_trunk`]
    /// for the port of `placeTrunk`/`generateBranch`.
    Cherry {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
        /// `branchCount` — `weighted_list` over `{1, 2, 3}` for every shipped
        /// cherry config.
        branch_count: IntProvider,
        /// `branchHorizontalLength`.
        branch_horizontal_length: IntProvider,
        /// `branchStartOffsetFromTop` — `(min, max)` inclusive. The SECOND
        /// branch's own start offset samples `UniformInt.of(min, max - 1)`,
        /// not this same range again — see [`place_cherry_trunk`]'s own doc
        /// on why the two draws use different bounds.
        branch_start_offset_from_top: (i32, i32),
        /// `branchEndOffsetFromTop`.
        branch_end_offset_from_top: IntProvider,
    },
    /// `UpwardsBranchingTrunkPlacer` — mangrove's real trunk: a
    /// single straight column, with a real chance (per log, per level except
    /// the top) of budding a short horizontal branch that grows its own
    /// foliage attachment. See [`place_upwards_branching_trunk`] for the port
    /// of `placeTrunk`/`placeBranch`.
    UpwardsBranching {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
        /// `extraBranchSteps`.
        extra_branch_steps: IntProvider,
        /// `placeBranchPerLogProbability`.
        place_branch_per_log_probability: f32,
        /// `extraBranchLength`.
        extra_branch_length: IntProvider,
    },
}

impl TrunkPlacerCfg {
pub(super)     fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        let base_height = v["base_height"].as_i64()? as i32;
        let height_rand_a = v["height_rand_a"].as_i64()? as i32;
        let height_rand_b = v["height_rand_b"].as_i64()? as i32;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "straight_trunk_placer" => Some(Self::Straight { base_height, height_rand_a, height_rand_b }),
            "forking_trunk_placer" => Some(Self::Forking { base_height, height_rand_a, height_rand_b }),
            "dark_oak_trunk_placer" => Some(Self::DarkOak { base_height, height_rand_a, height_rand_b }),
            "giant_trunk_placer" => Some(Self::Giant { base_height, height_rand_a, height_rand_b }),
            "mega_jungle_trunk_placer" => Some(Self::MegaJungle { base_height, height_rand_a, height_rand_b }),
            "fancy_trunk_placer" => Some(Self::Fancy { base_height, height_rand_a, height_rand_b }),
            "cherry_trunk_placer" => {
                let branch_start = &v["branch_start_offset_from_top"];
                let branch_start_offset_from_top = (
                    branch_start["min_inclusive"].as_i64()? as i32,
                    branch_start["max_inclusive"].as_i64()? as i32,
                );
                Some(Self::Cherry {
                    base_height,
                    height_rand_a,
                    height_rand_b,
                    branch_count: try_parse_int_provider(&v["branch_count"])?,
                    branch_horizontal_length: try_parse_int_provider(&v["branch_horizontal_length"])?,
                    branch_start_offset_from_top,
                    branch_end_offset_from_top: try_parse_int_provider(&v["branch_end_offset_from_top"])?,
                })
            }
            "upwards_branching_trunk_placer" => Some(Self::UpwardsBranching {
                base_height,
                height_rand_a,
                height_rand_b,
                extra_branch_steps: try_parse_int_provider(&v["extra_branch_steps"])?,
                place_branch_per_log_probability: v["place_branch_per_log_probability"].as_f64()? as f32,
                extra_branch_length: try_parse_int_provider(&v["extra_branch_length"])?,
            }),
            _ => None,
        }
    }

    fn heights(&self) -> (i32, i32, i32) {
        match self {
            Self::Straight { base_height, height_rand_a, height_rand_b }
            | Self::Forking { base_height, height_rand_a, height_rand_b }
            | Self::DarkOak { base_height, height_rand_a, height_rand_b }
            | Self::Giant { base_height, height_rand_a, height_rand_b }
            | Self::MegaJungle { base_height, height_rand_a, height_rand_b }
            | Self::Fancy { base_height, height_rand_a, height_rand_b }
            | Self::Cherry { base_height, height_rand_a, height_rand_b, .. }
            | Self::UpwardsBranching { base_height, height_rand_a, height_rand_b, .. } => {
                (*base_height, *height_rand_a, *height_rand_b)
            }
        }
    }

    /// `TrunkPlacer.getTreeHeight` — shared across every subclass (not
    /// overridden by `ForkingTrunkPlacer`, `DarkOakTrunkPlacer`, etc. in
    /// real vanilla either).
pub(super)     fn get_tree_height<R: RandomSource>(&self, random: &mut R) -> i32 {
        let (base_height, height_rand_a, height_rand_b) = self.heights();
        base_height + random.next_int_bounded(height_rand_a + 1) + random.next_int_bounded(height_rand_b + 1)
    }
}

/// `FoliagePlacer.FoliageAttachment` — one trunk-placement result the
/// foliage placer runs `create_foliage` against. [`TrunkPlacerCfg::Straight`]
/// always produces exactly one; [`TrunkPlacerCfg::Forking`] can produce one
/// or two (the lean column always attaches if it placed any log at all; the
/// branch attaches only if its own direction differs from the lean's AND it
/// placed at least one log — see [`place_trunk`]).
#[derive(Clone, Copy, Debug)]
pub(super) struct Attachment {
pub(super)     pos: BlockPos,
    /// `FoliageAttachment.radiusOffset` — nonzero only for
    /// `ForkingTrunkPlacer`'s primary (lean) attachment (`1`); every other
    /// attachment this module produces uses `0`. Consumed by
    /// [`FoliagePlacerCfg::Acacia`]'s `create_foliage`.
pub(super)     radius_offset: i32,
    /// `FoliageAttachment.doubleTrunk` — `true` only for
    /// [`TrunkPlacerCfg::DarkOak`]'s primary (2×2-trunk) attachment;
    /// `false` for every other attachment this module produces (`Straight`,
    /// `Forking`, and DarkOak's branch attachments). Consumed by
    /// [`FoliagePlacerCfg::DarkOak`]'s `create_foliage`, which widens its
    /// rows by one in the positive direction (and applies a different skip
    /// rule) when it is set.
pub(super)     double_trunk: bool,
}

/// `ForkingTrunkPlacer.placeTrunk` — acacia's real trunk.
/// Places `placeBelowTrunkBlock(origin.below())` first (matching
/// `StraightTrunkPlacer`'s own convention, [`place_tree`]'s existing
/// pre-loop call for the `Straight` case), then a single leaning log column
/// (`Direction.Plane.HORIZONTAL.getRandomDirection` = `random.nextInt(4)`
/// indexing `[NORTH, EAST, SOUTH, WEST]`, i.e. step vectors `(0,-1)`,
/// `(1,0)`, `(0,1)`, `(-1,0)` in that exact order — `Direction.java`'s own
/// `Plane.HORIZONTAL` face array), and then, only if a *second*,
/// independently-rolled direction differs from the first, a branch that
/// starts partway up the lean and runs for a few more logs in that second
/// direction. Both attachments are only added if `placeLog` actually placed
/// at least one log along that column (`OptionalInt` in the Java; `Option`
/// here) — an entirely-blocked lean or branch contributes no
/// [`Attachment`], matching vanilla exactly rather than attaching at a
/// position nothing was ever placed at.
/// Returns `(attachments, trunk_positions, placed_any)`. `trunk_positions`
/// is every position `trunkSetter`/`placeBelowTrunkBlock` was actually
/// invoked at (matching vanilla's real `trunks` set in `TreeFeature.place`)
/// — including the below-origin block, which real `placeBelowTrunkBlock`
/// places via the SAME `trunkSetter` (`TrunkPlacer.java`'s own
/// `placeBelowTrunkBlock`), and therefore counts as a real distance-0
/// source for [`update_leaf_distances`], not merely cosmetic soil.
/// Unit 8: `attachments` and `trunk_positions` are now caller-owned reusable
/// buffers (already cleared) rather than freshly allocated `Vec`s, and the return
/// is just `placed_any`. What is pushed, and in what order, is unchanged.
#[allow(clippy::too_many_arguments)]
pub(super) fn place_forking_trunk<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    below_trunk_provider: &Option<BlockStateProvider>,
    attachments: &mut Vec<Attachment>,
    trunk_positions: &mut Vec<BlockPos>,
) -> bool {
    if let Some(below_provider) = below_trunk_provider {
        let below_pos = BlockPos { x: origin.x, y: origin.y - 1, z: origin.z };
        if let Some(state) = below_provider.get_state_id(grid, tags, random, below_pos) {
            grid.set_id_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
            trunk_positions.push(below_pos);
        }
    }

    let mut placed_any = false;

    const STEP: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)]; // NORTH, EAST, SOUTH, WEST
    let lean_direction = STEP[random.next_int_bounded(4) as usize];
    let lean_height = tree_height - random.next_int_bounded(4) - 1;
    let mut lean_steps = 3 - random.next_int_bounded(3);
    let mut tx = origin.x;
    let mut tz = origin.z;
    let mut ey: Option<i32> = None;
    for yo in 0..tree_height {
        let yy = origin.y + yo;
        if yo >= lean_height && lean_steps > 0 {
            tx += lean_direction.0;
            tz += lean_direction.1;
            lean_steps -= 1;
        }
        let pos = BlockPos { x: tx, y: yy, z: tz };
        if valid_tree_pos(grid, tags, pos.x, pos.y, pos.z) {
            if let Some(state) = trunk_provider.get_state_id(grid, tags, random, pos) {
                grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
                placed_any = true;
                trunk_positions.push(pos);
                ey = Some(yy + 1);
            }
        }
    }
    if let Some(y) = ey {
        attachments.push(Attachment { pos: BlockPos { x: tx, y, z: tz }, radius_offset: 1, double_trunk: false });
    }

    tx = origin.x;
    tz = origin.z;
    let branch_direction = STEP[random.next_int_bounded(4) as usize];
    if branch_direction != lean_direction {
        let branch_pos = lean_height - random.next_int_bounded(2) - 1;
        let mut branch_steps = 1 + random.next_int_bounded(3);
        let mut ey: Option<i32> = None;
        let mut yo = branch_pos;
        while yo < tree_height && branch_steps > 0 {
            if yo >= 1 {
                let yy = origin.y + yo;
                tx += branch_direction.0;
                tz += branch_direction.1;
                let pos = BlockPos { x: tx, y: yy, z: tz };
                if valid_tree_pos(grid, tags, pos.x, pos.y, pos.z) {
                    if let Some(state) = trunk_provider.get_state_id(grid, tags, random, pos) {
                        grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
                        placed_any = true;
                        trunk_positions.push(pos);
                        ey = Some(yy + 1);
                    }
                }
            }
            branch_steps -= 1;
            yo += 1;
        }
        if let Some(y) = ey {
            attachments.push(Attachment { pos: BlockPos { x: tx, y, z: tz }, radius_offset: 0, double_trunk: false });
        }
    }

    placed_any
}

/// `TreeFeature.validTreePos`: air or `#minecraft:replaceable_by_trees`.
///
/// One grid read and two bit tests since Unit 8 — it was an interner read guard,
/// a `split('[')` and two `HashSet<String>` probes, evaluated once per candidate
/// log and once per candidate leaf. Written once here rather than inlined at each
/// of the six call sites it had, so the predicate cannot drift between trunk kinds.
pub(super) fn valid_tree_pos(grid: &VegGrid, tags: &VegTags, x: i32, y: i32, z: i32) -> bool {
    let id = grid.get_id(x, y, z);
    let interner = grid.interner();
    tags.has(interner, Tag::Air, id) || tags.has(interner, Tag::ReplaceableByTrees, id)
}

/// `DarkOakTrunkPlacer.placeTrunk` — dark oak's real trunk,
/// also the trunk of pale oak (both use `dark_oak_trunk_placer`). A 2×2 log
/// column: four `placeBelowTrunkBlock`s at the origin's `(0,0)`, `east`,
/// `south`, `south().east()` base, then per level (gated by the anchor's
/// `TreeFeature.isAirOrLeaves` — a dark oak trunk can grow up through a
/// neighbour's already-placed canopy, which dense dark forests depend on) up
/// to four `placeLog`s at the same 2×2 footprint, the whole column leaning
/// one step for its upper portion (`leanHeight`/`leanSteps`, the same
/// `Direction.Plane.HORIZONTAL.getRandomDirection` indexing
/// [`place_forking_trunk`]'s `STEP` table). Around the top, up to a few
/// short hanging branches descend from just below `ey` and end in their own
/// [`Attachment`]s. The primary attachment is `double_trunk: true` (the 2×2
/// footprint — consumed by [`FoliagePlacerCfg::DarkOak`]); branch
/// attachments are `false`.
/// Returns `(attachments, trunk_positions, placed_any)` in the same shape as
/// [`place_forking_trunk`] — `trunk_positions` is every position
/// `trunkSetter`/`placeBelowTrunkBlock` fired at (the below-origin 2×2
/// included), seeding [`update_leaf_distances`]'s BFS.
/// Unit 8: same buffer-in / `bool`-out change as [`place_forking_trunk`].
#[allow(clippy::too_many_arguments)]
pub(super) fn place_dark_oak_trunk<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    below_trunk_provider: &Option<BlockStateProvider>,
    attachments: &mut Vec<Attachment>,
    trunk_positions: &mut Vec<BlockPos>,
) -> bool {
    // The 2×2 base: `below`, `below.east()`, `below.south()`,
    // `below.south().east()` — each via `placeBelowTrunkBlock`, in exactly
    // vanilla's order.
    for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        if let Some(below_provider) = below_trunk_provider {
            let below_pos = BlockPos {
                x: origin.x + dx,
                y: origin.y - 1,
                z: origin.z + dz,
            };
            if let Some(state) = below_provider.get_state_id(grid, tags, random, below_pos) {
                grid.set_id_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
                trunk_positions.push(below_pos);
            }
        }
    }

    let mut placed_any = false;

    const STEP: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)]; // NORTH, EAST, SOUTH, WEST
    let lean_direction = STEP[random.next_int_bounded(4) as usize];
    let lean_height = tree_height - random.next_int_bounded(4);
    let mut lean_steps = 2 - random.next_int_bounded(3);
    let mut tx = origin.x;
    let mut tz = origin.z;
    let ey = origin.y + tree_height - 1;

    for dy in 0..tree_height {
        if dy >= lean_height && lean_steps > 0 {
            tx += lean_direction.0;
            tz += lean_direction.1;
            lean_steps -= 1;
        }
        let yy = origin.y + dy;
        // `TreeFeature.isAirOrLeaves` at the anchor — the outer gate before
        // the four `placeLog` calls; each log itself still individually
        // checks `validTreePos` (air or `#replaceable_by_trees`) below,
        // matching vanilla exactly.
        let anchor = grid.get_id(tx, yy, tz);
        if tags.has(grid.interner(), Tag::Air, anchor)
            || tags.has(grid.interner(), Tag::Leaves, anchor)
        {
            for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let (lx, lz) = (tx + dx, tz + dz);
                if valid_tree_pos(grid, tags, lx, yy, lz) {
                    if let Some(state) = trunk_provider.get_state_id(grid, tags, random, BlockPos { x: lx, y: yy, z: lz }) {
                        grid.set_id_if_in_bounds(lx, yy, lz, state);
                        placed_any = true;
                        trunk_positions.push(BlockPos { x: lx, y: yy, z: lz });
                    }
                }
            }
        }
    }

    attachments.push(Attachment {
        pos: BlockPos { x: tx, y: ey, z: tz },
        radius_offset: 0,
        double_trunk: true,
    });

    // The hanging branches: a 4×4 loop over `ox`/`oz` in `-1..=2`; only the
    // 12 cells outside the trunk's own 2×2 interior roll
    // `random.nextInt(3)` (`&&` short-circuits on the interior, so those 4
    // cells draw nothing), and on a `0` a branch of `nextInt(3) + 2` logs
    // descends from `ey - 1`.
    for ox in -1..=2 {
        for oz in -1..=2 {
            if (ox < 0 || ox > 1 || oz < 0 || oz > 1) && random.next_int_bounded(3) <= 0 {
                let length = random.next_int_bounded(3) + 2;
                let (bx, bz) = (origin.x + ox, origin.z + oz);
                for branch_y in 0..length {
                    let by = ey - branch_y - 1;
                    if valid_tree_pos(grid, tags, bx, by, bz) {
                        if let Some(state) = trunk_provider.get_state_id(grid, tags, random, BlockPos { x: bx, y: by, z: bz }) {
                            grid.set_id_if_in_bounds(bx, by, bz, state);
                            placed_any = true;
                            trunk_positions.push(BlockPos { x: bx, y: by, z: bz });
                        }
                    }
                }
                attachments.push(Attachment {
                    pos: BlockPos { x: bx, y: ey, z: bz },
                    radius_offset: 0,
                    double_trunk: false,
                });
            }
        }
    }

    placed_any
}

/// `GiantTrunkPlacer.placeTrunk` — the redwood/giant-spruce trunk (added
/// with the savanna/acacia increment), and the shared base [`place_mega_jungle_trunk`] calls via its own
/// `super.placeTrunk`. A static 2×2 log column: four `placeBelowTrunkBlock`s
/// at the origin's `(0,0)`, east, south, south+east base (the same order as
/// [`place_dark_oak_trunk`]'s own base), then per level `0..tree_height` a
/// `placeLogIfFree` at `(0,0)`, plus — only for every level EXCEPT the last
/// (`hh < tree_height - 1`) — three more at `(1,0)`, `(1,1)`, `(0,1)`, so the
/// column is a full 2×2 for every row but its very top.
///
/// No lean, and no `isAirOrLeaves` anchor gate the way [`place_dark_oak_trunk`]
/// has one. `GiantTrunkPlacer.placeLogIfFree`'s own gate (`isFree` — real
/// `validTreePos` OR "already a log") is provably equivalent here to gating
/// purely on [`valid_tree_pos`]: `placeLogIfFree` calls `placeLog`
/// unconditionally once `isFree` passes, and `placeLog` itself re-checks
/// `validTreePos` before ever drawing from `trunk_provider` or writing — so
/// the "OR already a log" half of `isFree` can only ever gate a call that
/// then draws nothing and writes nothing. Neither half of the gate chain
/// (`isFree` or `validTreePos`) consumes RNG on its own; only
/// `trunk_provider.get_state_id` conditionally does, exactly as in every
/// other trunk placer in this module.
///
/// Returns a single [`Attachment`] at `origin.above(tree_height)`,
/// `double_trunk: true` — consumed by [`FoliagePlacerCfg::MegaJungle`] and
/// [`FoliagePlacerCfg::MegaPine`], both of which read `radius_offset`/
/// `double_trunk` off it exactly like [`place_dark_oak_trunk`]'s primary
/// attachment.
#[allow(clippy::too_many_arguments)]
pub(super) fn place_giant_trunk<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    below_trunk_provider: &Option<BlockStateProvider>,
    attachments: &mut Vec<Attachment>,
    trunk_positions: &mut Vec<BlockPos>,
) -> bool {
    for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        if let Some(below_provider) = below_trunk_provider {
            let below_pos = BlockPos {
                x: origin.x + dx,
                y: origin.y - 1,
                z: origin.z + dz,
            };
            if let Some(state) = below_provider.get_state_id(grid, tags, random, below_pos) {
                grid.set_id_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
                trunk_positions.push(below_pos);
            }
        }
    }

    let mut placed_any = false;
    for hh in 0..tree_height {
        let yy = origin.y + hh;
        // `(0,0)` always fires; the other three cells of the 2×2 only fire
        // for every level except the topmost — `GiantTrunkPlacer.placeTrunk`'s
        // own `hh < treeHeight - 1` guard.
        const WIDE: [(i32, i32); 4] = [(0, 0), (1, 0), (1, 1), (0, 1)];
        let cells: &[(i32, i32)] = if hh < tree_height - 1 { &WIDE } else { &WIDE[..1] };
        for &(dx, dz) in cells {
            let pos = BlockPos { x: origin.x + dx, y: yy, z: origin.z + dz };
            if valid_tree_pos(grid, tags, pos.x, pos.y, pos.z) {
                if let Some(state) = trunk_provider.get_state_id(grid, tags, random, pos) {
                    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
                    placed_any = true;
                    trunk_positions.push(pos);
                }
            }
        }
    }

    attachments.push(Attachment {
        pos: BlockPos { x: origin.x, y: origin.y + tree_height, z: origin.z },
        radius_offset: 0,
        double_trunk: true,
    });

    placed_any
}

/// `MegaJungleTrunkPlacer.placeTrunk` — mega jungle's real trunk (added
/// with the savanna/acacia increment): [`place_giant_trunk`]'s own 2×2 column (`super.placeTrunk` in real
/// vanilla — `MegaJungleTrunkPlacer extends GiantTrunkPlacer`), then a spiral
/// of short radial branches climbing the trunk's upper half.
///
/// RNG order, matching the real Java `for` loop's desugaring exactly (`init`
/// once, then `while(cond) { body; update }`): `branch_height = tree_height -
/// 2 - random.nextInt(4)` is drawn exactly once (the loop's `init` clause);
/// `branch_height -= 2 + random.nextInt(4)` fires at the END of every
/// iteration's body (the loop's `update` clause) — **including** the
/// iteration whose resulting `branch_height` fails the next `>
/// tree_height / 2` check and ends the loop, so that final draw is real and
/// must not be skipped by restructuring this as a Rust `while let` that only
/// draws when about to run the body again.
///
/// Each branch draws one `nextFloat` angle, then walks 5 steps outward along
/// `(cos(angle), sin(angle))` — vanilla's own sine **table**
/// ([`lodestone_worldgen_core::math::cos`]/`sin`), not `f32::cos`/`sin`: the
/// two are not guaranteed to land on the same integer after the `(int)`
/// truncation immediately below, and this is the first placer in this module
/// where that distinction is load-bearing (every earlier trunk/foliage
/// placer's geometry is integer arithmetic with no trig at all). Each step
/// places via `placeLog` directly — no `isFree`/anchor gate at all, draws and
/// places exactly when [`valid_tree_pos`], same as every other unconditional
/// `placeLog` call in this module.
#[allow(clippy::too_many_arguments)]
pub(super) fn place_mega_jungle_trunk<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    below_trunk_provider: &Option<BlockStateProvider>,
    attachments: &mut Vec<Attachment>,
    trunk_positions: &mut Vec<BlockPos>,
) -> bool {
    let mut placed_any = place_giant_trunk(
        random,
        origin,
        tree_height,
        grid,
        tags,
        trunk_provider,
        below_trunk_provider,
        attachments,
        trunk_positions,
    );

    // `(float) (Math.PI * 2)`: the multiplication happens in `double`
    // precision and is narrowed to `float` only at the end — narrowing first
    // and multiplying second would round differently.
    let two_pi = (2.0_f64 * std::f64::consts::PI) as f32;

    let mut branch_height = tree_height - 2 - random.next_int_bounded(4);
    while branch_height > tree_height / 2 {
        let angle = random.next_float() * two_pi;
        let mut bx = 0i32;
        let mut bz = 0i32;
        for b in 0..5i32 {
            // `Mth.cos`/`Mth.sin` take `double` — `angle` (a `float`) widens
            // exactly, matching Java's implicit widening at the call site.
            bx = (1.5_f32 + lodestone_worldgen_core::math::cos(angle as f64) * b as f32) as i32;
            bz = (1.5_f32 + lodestone_worldgen_core::math::sin(angle as f64) * b as f32) as i32;
            let pos = BlockPos {
                x: origin.x + bx,
                y: origin.y + branch_height - 3 + b / 2,
                z: origin.z + bz,
            };
            if valid_tree_pos(grid, tags, pos.x, pos.y, pos.z) {
                if let Some(state) = trunk_provider.get_state_id(grid, tags, random, pos) {
                    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
                    placed_any = true;
                    trunk_positions.push(pos);
                }
            }
        }
        attachments.push(Attachment {
            pos: BlockPos { x: origin.x + bx, y: origin.y + branch_height, z: origin.z + bz },
            radius_offset: -2,
            double_trunk: false,
        });
        branch_height -= 2 + random.next_int_bounded(4);
    }

    placed_any
}

/// `TrunkPlacer.isFree` — `validTreePos(pos) || pos is a log`. Every other
/// placer in this module only ever needs [`valid_tree_pos`] on its own
/// (argued case-by-case in each one's own doc comment: the OR-with-logs
/// clause never changes the outcome because the subsequent `placeLog` call
/// re-checks `validTreePos` regardless). [`place_fancy_trunk`]'s dry-run
/// limb check is the first place in this module where that argument does
/// NOT apply — `makeLimb`'s `doPlace: false` branch uses `isFree` directly
/// as its own accept/reject verdict, with no follow-up `placeLog` to fall
/// back on, so the "OR already a log" half is load-bearing here.
fn is_free(grid: &VegGrid, tags: &VegTags, x: i32, y: i32, z: i32) -> bool {
    valid_tree_pos(grid, tags, x, y, z) || tags.has(grid.interner(), Tag::Logs, grid.get_id(x, y, z))
}

/// `FancyTrunkPlacer.getLogAxis`: the log axis a limb step should carry,
/// derived from how far this step has moved horizontally from the limb's
/// own start — `Axis::Y` only when the step hasn't moved horizontally at
/// all (`maxdiff == 0`), matching vanilla exactly (not merely "usually Y for
/// a vertical trunk").
fn fancy_log_axis(start_pos: BlockPos, pos: BlockPos) -> &'static str {
    let xdiff = (pos.x - start_pos.x).abs();
    let zdiff = (pos.z - start_pos.z).abs();
    let maxdiff = xdiff.max(zdiff);
    if maxdiff > 0 {
        if xdiff == maxdiff { "x" } else { "z" }
    } else {
        "y"
    }
}

/// `FancyTrunkPlacer.makeLimb` — walks the straight line from `start_pos` to
/// `end_pos` in exactly `getSteps(delta)` = `max(|dx|,|dy|,|dz|)` increments
/// (so every limb, however diagonal, visits its endpoints exactly and
/// distributes evenly between them), either placing a real log at each step
/// (`do_place: true`, unconditionally through the whole line) or checking
/// [`is_free`] at each step and bailing the moment one fails (`do_place:
/// false`, a pure dry run — real vanilla's own "is there room for this
/// limb" probe, called with the identical `start_pos`/`end_pos` pair the
/// placing call would use later).
///
/// The `(float) delta / steps` division is real IEEE float division, not
/// integer division — `steps` can only be `0` when `do_place` is `true` and
/// the two endpoints coincide (guarded by the caller never doing that; see
/// [`place_fancy_trunk`]'s own call sites), but the division is written to
/// match Java's `0.0f/0` = NaN / `x/0.0f` = ±Infinity semantics rather than
/// risk an integer-division panic if that guarantee is ever loosened.
#[allow(clippy::too_many_arguments)]
fn make_limb<R: RandomSource>(
    random: &mut R,
    start_pos: BlockPos,
    end_pos: BlockPos,
    do_place: bool,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    placed_any: &mut bool,
    trunk_positions: &mut Vec<BlockPos>,
) -> bool {
    if !do_place && start_pos == end_pos {
        return true;
    }
    let delta = (end_pos.x - start_pos.x, end_pos.y - start_pos.y, end_pos.z - start_pos.z);
    let steps = delta.0.abs().max(delta.1.abs()).max(delta.2.abs());
    let steps_f = steps as f32;
    let dxf = delta.0 as f32 / steps_f;
    let dyf = delta.1 as f32 / steps_f;
    let dzf = delta.2 as f32 / steps_f;
    for i in 0..=steps {
        let fi = i as f32;
        let pos = BlockPos {
            x: start_pos.x + lodestone_worldgen_core::math::floor(f64::from(0.5_f32 + fi * dxf)),
            y: start_pos.y + lodestone_worldgen_core::math::floor(f64::from(0.5_f32 + fi * dyf)),
            z: start_pos.z + lodestone_worldgen_core::math::floor(f64::from(0.5_f32 + fi * dzf)),
        };
        if do_place {
            if valid_tree_pos(grid, tags, pos.x, pos.y, pos.z) {
                if let Some(state) = trunk_provider.get_state_id(grid, tags, random, pos) {
                    let axis = fancy_log_axis(start_pos, pos);
                    let state = tags
                        .rewrite(grid.interner(), state, Rewrite::Axis(axis))
                        .unwrap_or(state);
                    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
                    *placed_any = true;
                    trunk_positions.push(pos);
                }
            }
        } else if !is_free(grid, tags, pos.x, pos.y, pos.z) {
            return false;
        }
    }
    true
}

/// `FancyTrunkPlacer.treeShape`: the limb-spray envelope radius at height
/// `y` of a tree whose total (already `+2`-adjusted) height is `height` —
/// `-1.0` below `0.3 * height` (no limbs grow that low), `0.0` once the
/// implied circle would extend past its own centre, and a real ellipse
/// radius (halved) in between. `distance` is computed unconditionally
/// before either branch, matching vanilla's own evaluation order exactly
/// (including that it can transiently hold a value from a negative
/// `sqrt` argument on the `adjacent == 0.0` path, which is immediately
/// overwritten rather than read).
fn tree_shape(height: i32, y: i32) -> f32 {
    if (y as f32) < (height as f32) * 0.3 {
        return -1.0;
    }
    let radius = height as f32 / 2.0;
    let adjacent = radius - y as f32;
    let mut distance = (f64::from(radius * radius - adjacent * adjacent).sqrt()) as f32;
    if adjacent == 0.0 {
        distance = radius;
    } else if lodestone_worldgen_core::math::abs_f32(adjacent) >= radius {
        return 0.0;
    }
    distance * 0.5
}

/// `FancyTrunkPlacer.trimBranches`: whether a branch based this low
/// (`local_y`, relative to the tree's own origin) survives into the final
/// foliage-attachment list at all — `local_y >= height * 0.2`, in `f64`
/// (Java widens both operands via the `double` literal `0.2`).
fn trim_branches(height: i32, local_y: i32) -> bool {
    f64::from(local_y) >= f64::from(height) * 0.2
}

/// `FancyTrunkPlacer.placeTrunk` — oak's `fancy_oak_*`/`fancy_oak_checked`
/// branch (this change's highest-value remaining gap): a slim central trunk,
/// plus a scattered spray of short diagonal "check" limbs (one candidate
/// per `relativeY` level counting down from `height - 5`, gated by
/// [`tree_shape`]'s envelope), each of which only actually grows — as a
/// `checkBranchBase → checkStart` limb rooted back at the main trunk — if
/// its own outward dry-run limb *and* its inward branch-to-trunk dry-run
/// limb both find room via [`is_free`]. Foliage attaches at every
/// `checkStart` whose owning branch survives [`trim_branches`], including
/// the tree's own always-present first entry (`origin.above(height - 5)`,
/// paired with `trunk_top` as its own "branch base" — added before the walk
/// even starts, exactly matching vanilla's unconditional first push).
///
/// `clusters_per_y` (`Math.min(1, Mth.floor(1.382 + (height/13.0)^2))`) is
/// carried as vanilla's own formula rather than inlined as the literal `1`
/// it always evaluates to for every `height >= 0` (the squared term can only
/// push the sum higher, never below `1.382`, so the `floor` is always `>= 1`
/// and `min(1, …)` is always exactly `1`) — ported for fidelity to the real
/// class, not because this module has observed a `height` where it differs.
///
/// RNG draw order: `place_below_trunk_block` (draws only if a below-trunk
/// provider is configured and its own provider draws), then exactly TWO
/// `next_float` calls per `relative_y` level whose [`tree_shape`] is
/// non-negative (radius, then angle — in that order, and drawn even when
/// both of that level's dry-run limb checks go on to fail), then the real
/// per-log draws inside the main trunk's [`make_limb`] call, then the real
/// per-log draws inside each surviving branch's own [`make_limb`] call, in
/// `foliage_coords`' own insertion order (branch spray order, tree-origin
/// entry first). `Math.sin`/`Math.cos` here are the REAL, unbounded
/// `f64::sin`/`cos` — **not** [`lodestone_worldgen_core::math::sin`]/`cos`
/// (the 65536-entry table): `FancyTrunkPlacer.placeTrunk` calls
/// `Math.sin(angle)`/`Math.cos(angle)` directly, unlike
/// [`place_mega_jungle_trunk`]'s branch geometry, which really does go
/// through `Mth.sin`/`Mth.cos`. Verified against `.cache/mc/26.2/src`
/// rather than assumed from that placer's own precedent.
#[allow(clippy::too_many_arguments)]
pub(super) fn place_fancy_trunk<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    below_trunk_provider: &Option<BlockStateProvider>,
    attachments: &mut Vec<Attachment>,
    trunk_positions: &mut Vec<BlockPos>,
) -> bool {
    if let Some(below_provider) = below_trunk_provider {
        let below_pos = BlockPos { x: origin.x, y: origin.y - 1, z: origin.z };
        if let Some(state) = below_provider.get_state_id(grid, tags, random, below_pos) {
            grid.set_id_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
            trunk_positions.push(below_pos);
        }
    }

    let mut placed_any = false;

    let height = tree_height + 2;
    let trunk_height = lodestone_worldgen_core::math::floor(f64::from(height) * 0.618);
    let clusters_per_y = 1i32.min(lodestone_worldgen_core::math::floor(
        1.382 + (f64::from(height) / 13.0).powi(2),
    ));
    let trunk_top = origin.y + trunk_height;
    let mut relative_y = height - 5;

    struct FoliageCoord {
        pos: BlockPos,
        branch_base: i32,
    }
    let mut foliage_coords: Vec<FoliageCoord> = Vec::new();
    foliage_coords.push(FoliageCoord {
        pos: BlockPos { x: origin.x, y: origin.y + relative_y, z: origin.z },
        branch_base: trunk_top,
    });

    while relative_y >= 0 {
        let shape = tree_shape(height, relative_y);
        if shape >= 0.0 {
            for _ in 0..clusters_per_y.max(0) {
                // `1.0 * treeShape * (nextFloat() + 0.328)` — the whole
                // expression is `double` (the `1.0`/`0.328` literals force
                // it), but `angle` below is NOT: Java's
                // `nextFloat() * 2.0F * Math.PI` multiplies in `float`
                // first, THEN promotes to `double` for the `Math.PI` term —
                // preserved here as two separate casts, not one combined
                // `f64` multiply, because that changes the rounding.
                let radius = 1.0_f64 * f64::from(shape) * (f64::from(random.next_float()) + 0.328);
                let angle = f64::from(random.next_float() * 2.0_f32) * std::f64::consts::PI;
                let x = radius * angle.sin() + 0.5;
                let z = radius * angle.cos() + 0.5;
                let check_start = BlockPos {
                    x: origin.x + lodestone_worldgen_core::math::floor(x),
                    y: origin.y + relative_y - 1,
                    z: origin.z + lodestone_worldgen_core::math::floor(z),
                };
                let check_end = BlockPos { x: check_start.x, y: check_start.y + 5, z: check_start.z };
                if make_limb(
                    random, check_start, check_end, false, grid, tags, trunk_provider, &mut placed_any,
                    trunk_positions,
                ) {
                    let dx = origin.x - check_start.x;
                    let dz = origin.z - check_start.z;
                    let sum_sq = dx * dx + dz * dz;
                    let branch_height = f64::from(check_start.y) - f64::from(sum_sq).sqrt() * 0.381;
                    let branch_top = if branch_height > f64::from(trunk_top) {
                        trunk_top
                    } else {
                        branch_height as i32
                    };
                    let check_branch_base = BlockPos { x: origin.x, y: branch_top, z: origin.z };
                    if make_limb(
                        random, check_branch_base, check_start, false, grid, tags, trunk_provider,
                        &mut placed_any, trunk_positions,
                    ) {
                        foliage_coords.push(FoliageCoord { pos: check_start, branch_base: branch_top });
                    }
                }
            }
        }
        relative_y -= 1;
    }

    // The real main trunk — a straight vertical limb from `origin` up
    // `trunk_height` blocks, placed unconditionally (`do_place: true`).
    make_limb(
        random,
        origin,
        BlockPos { x: origin.x, y: origin.y + trunk_height, z: origin.z },
        true,
        grid,
        tags,
        trunk_provider,
        &mut placed_any,
        trunk_positions,
    );

    // `makeBranches`: for every foliage coord whose own base differs from
    // its attachment position AND survives `trim_branches`, grow the real
    // branch limb from the trunk out to that attachment.
    for fc in &foliage_coords {
        let base_coord = BlockPos { x: origin.x, y: fc.branch_base, z: origin.z };
        if base_coord != fc.pos && trim_branches(height, fc.branch_base - origin.y) {
            make_limb(
                random, base_coord, fc.pos, true, grid, tags, trunk_provider, &mut placed_any,
                trunk_positions,
            );
        }
    }

    for fc in &foliage_coords {
        if trim_branches(height, fc.branch_base - origin.y) {
            attachments.push(Attachment { pos: fc.pos, radius_offset: 0, double_trunk: false });
        }
    }

    placed_any
}

/// `Direction.getAxis()` for one of [`place_forking_trunk`]'s `STEP` vectors —
/// `x` for EAST/WEST, `z` for NORTH/SOUTH. Shared by [`place_cherry_trunk`]'s
/// `sidewaysStateModifier` (`RotatedPillarBlock.AXIS`).
fn horizontal_axis(direction: (i32, i32)) -> &'static str {
    if direction.0 != 0 { "x" } else { "z" }
}

/// `CherryTrunkPlacer.generateBranch` — walks one side branch out from the
/// trunk to a randomly-chosen end position, alternating a per-step coin flip
/// (weighted by how much vertical vs. horizontal distance remains) between
/// climbing and reaching outward. Every log placed while moving horizontally
/// gets its axis rewritten to `branch_direction`'s axis
/// ([`horizontal_axis`]); every log placed while climbing keeps the
/// trunk_provider's own (vertical) axis untouched — matching
/// `Function.identity()` vs. `sidewaysStateModifier` in the real Java
/// exactly.
#[allow(clippy::too_many_arguments)]
fn generate_cherry_branch<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    branch_direction: (i32, i32),
    offset_from_origin: i32,
    middle_continues_upwards: bool,
    branch_horizontal_length: &IntProvider,
    branch_end_offset_from_top: &IntProvider,
    placed_any: &mut bool,
    trunk_positions: &mut Vec<BlockPos>,
) -> Attachment {
    let axis = horizontal_axis(branch_direction);
    let mut log_pos = (origin.x, origin.y + offset_from_origin, origin.z);
    let branch_end_pos_offset_from_origin = tree_height - 1 + branch_end_offset_from_top.sample(random);
    let extend_branch_away_from_trunk =
        middle_continues_upwards || branch_end_pos_offset_from_origin < offset_from_origin;
    let distance_to_trunk =
        branch_horizontal_length.sample(random) + i32::from(extend_branch_away_from_trunk);
    let branch_end_pos = (
        origin.x + branch_direction.0 * distance_to_trunk,
        origin.y + branch_end_pos_offset_from_origin,
        origin.z + branch_direction.1 * distance_to_trunk,
    );
    let steps_horizontally = if extend_branch_away_from_trunk { 2 } else { 1 };

    let mut place_sideways = |random: &mut R, pos: (i32, i32, i32), grid: &mut VegGrid, placed_any: &mut bool, trunk_positions: &mut Vec<BlockPos>| {
        let bp = BlockPos { x: pos.0, y: pos.1, z: pos.2 };
        if valid_tree_pos(grid, tags, bp.x, bp.y, bp.z) {
            if let Some(state) = trunk_provider.get_state_id(grid, tags, random, bp) {
                let state = tags.rewrite(grid.interner(), state, Rewrite::Axis(axis)).unwrap_or(state);
                grid.set_id_if_in_bounds(bp.x, bp.y, bp.z, state);
                *placed_any = true;
                trunk_positions.push(bp);
            }
        }
    };
    let place_vertical = |random: &mut R, pos: (i32, i32, i32), grid: &mut VegGrid, placed_any: &mut bool, trunk_positions: &mut Vec<BlockPos>| {
        let bp = BlockPos { x: pos.0, y: pos.1, z: pos.2 };
        if valid_tree_pos(grid, tags, bp.x, bp.y, bp.z) {
            if let Some(state) = trunk_provider.get_state_id(grid, tags, random, bp) {
                grid.set_id_if_in_bounds(bp.x, bp.y, bp.z, state);
                *placed_any = true;
                trunk_positions.push(bp);
            }
        }
    };

    for _ in 0..steps_horizontally {
        log_pos.0 += branch_direction.0;
        log_pos.2 += branch_direction.1;
        place_sideways(random, log_pos, grid, placed_any, trunk_positions);
    }

    let vertical_direction: i32 = if branch_end_pos.1 > log_pos.1 { 1 } else { -1 };

    loop {
        let distance = (log_pos.0 - branch_end_pos.0).abs()
            + (log_pos.1 - branch_end_pos.1).abs()
            + (log_pos.2 - branch_end_pos.2).abs();
        if distance == 0 {
            return Attachment {
                pos: BlockPos { x: branch_end_pos.0, y: branch_end_pos.1 + 1, z: branch_end_pos.2 },
                radius_offset: 0,
                double_trunk: false,
            };
        }
        let chance_to_grow_vertically = (branch_end_pos.1 - log_pos.1).abs() as f32 / distance as f32;
        let grow_vertically = random.next_float() < chance_to_grow_vertically;
        if grow_vertically {
            log_pos.1 += vertical_direction;
            place_vertical(random, log_pos, grid, placed_any, trunk_positions);
        } else {
            log_pos.0 += branch_direction.0;
            log_pos.2 += branch_direction.1;
            place_sideways(random, log_pos, grid, placed_any, trunk_positions);
        }
    }
}

/// `CherryTrunkPlacer.placeTrunk` — cherry's real trunk. A
/// single straight column (whose height depends on how many branches there
/// will be — the full `tree_height` only if there is a middle branch), plus
/// one branch always, a second (opposite-direction) branch if `branch_count
/// >= 2`, and a synthetic middle attachment directly above the trunk if
/// `branch_count == 3`. `secondBranchStartOffsetFromTop` samples
/// `UniformInt.of(min, max - 1)` — a NARROWER range than the first branch's
/// own `branch_start_offset_from_top`, not the same one redrawn — and if the
/// resulting offset ties-or-exceeds the first branch's, it is bumped by one
/// so the two branches never start at the same height.
#[allow(clippy::too_many_arguments)]
pub(super) fn place_cherry_trunk<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    below_trunk_provider: &Option<BlockStateProvider>,
    branch_count: &IntProvider,
    branch_horizontal_length: &IntProvider,
    branch_start_offset_from_top: (i32, i32),
    branch_end_offset_from_top: &IntProvider,
    attachments: &mut Vec<Attachment>,
    trunk_positions: &mut Vec<BlockPos>,
) -> bool {
    if let Some(below_provider) = below_trunk_provider {
        let below_pos = BlockPos { x: origin.x, y: origin.y - 1, z: origin.z };
        if let Some(state) = below_provider.get_state_id(grid, tags, random, below_pos) {
            grid.set_id_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
            trunk_positions.push(below_pos);
        }
    }

    let (bs_min, bs_max) = branch_start_offset_from_top;
    let first_branch_offset_from_origin =
        (tree_height - 1 + lodestone_worldgen_core::math::random_between_inclusive(random, bs_min, bs_max)).max(0);
    // `UniformInt.of(min, max - 1)` — a genuinely narrower range, not the
    // same `(bs_min, bs_max)` redrawn. See this function's own doc.
    let mut second_branch_offset_from_origin = (tree_height - 1
        + lodestone_worldgen_core::math::random_between_inclusive(random, bs_min, bs_max - 1))
    .max(0);
    if second_branch_offset_from_origin >= first_branch_offset_from_origin {
        second_branch_offset_from_origin += 1;
    }

    let branch_count_n = branch_count.sample(random);
    let has_middle_branch = branch_count_n == 3;
    let has_both_side_branches = branch_count_n >= 2;
    let trunk_height = if has_middle_branch {
        tree_height
    } else if has_both_side_branches {
        first_branch_offset_from_origin.max(second_branch_offset_from_origin) + 1
    } else {
        first_branch_offset_from_origin + 1
    };

    let mut placed_any = false;
    for y in 0..trunk_height {
        let pos = BlockPos { x: origin.x, y: origin.y + y, z: origin.z };
        if valid_tree_pos(grid, tags, pos.x, pos.y, pos.z) {
            if let Some(state) = trunk_provider.get_state_id(grid, tags, random, pos) {
                grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
                placed_any = true;
                trunk_positions.push(pos);
            }
        }
    }

    if has_middle_branch {
        attachments.push(Attachment {
            pos: BlockPos { x: origin.x, y: origin.y + trunk_height, z: origin.z },
            radius_offset: 0,
            double_trunk: false,
        });
    }

    const STEP: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)]; // NORTH, EAST, SOUTH, WEST
    let tree_direction = STEP[random.next_int_bounded(4) as usize];

    attachments.push(generate_cherry_branch(
        random,
        origin,
        tree_height,
        grid,
        tags,
        trunk_provider,
        tree_direction,
        first_branch_offset_from_origin,
        first_branch_offset_from_origin < trunk_height - 1,
        branch_horizontal_length,
        branch_end_offset_from_top,
        &mut placed_any,
        trunk_positions,
    ));

    if has_both_side_branches {
        let opposite = (-tree_direction.0, -tree_direction.1);
        attachments.push(generate_cherry_branch(
            random,
            origin,
            tree_height,
            grid,
            tags,
            trunk_provider,
            opposite,
            second_branch_offset_from_origin,
            second_branch_offset_from_origin < trunk_height - 1,
            branch_horizontal_length,
            branch_end_offset_from_top,
            &mut placed_any,
            trunk_positions,
        ));
    }

    placed_any
}

/// `UpwardsBranchingTrunkPlacer.placeTrunk` — mangrove's real trunk. A
/// single straight column, base-first (`placeBelowTrunkBlock`,
/// exactly like [`place_forking_trunk`]'s own convention), climbing
/// `tree_height` logs. At every level except the very top, a successfully
/// placed log has a `place_branch_per_log_probability` chance of budding a
/// short horizontal branch (`placeBranch`) that walks outward
/// `extra_branch_steps` times, each step attempted via [`valid_tree_pos`]
/// **extended** with `can_grow_through` (mangrove's real
/// `validTreePos` override — see [`place_upwards_branching_valid`]),
/// contributing one [`Attachment`] per branch step (not just at the end —
/// unlike every other trunk placer here) plus, if the branch climbed at all,
/// two more attachments at its tip and two below. The tree's own top always
/// gets a final attachment too.
#[allow(clippy::too_many_arguments)]
pub(super) fn place_upwards_branching_trunk<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    below_trunk_provider: &Option<BlockStateProvider>,
    extra_branch_steps: &IntProvider,
    place_branch_per_log_probability: f32,
    extra_branch_length: &IntProvider,
    can_grow_through: Tag,
    attachments: &mut Vec<Attachment>,
    trunk_positions: &mut Vec<BlockPos>,
) -> bool {
    if let Some(below_provider) = below_trunk_provider {
        let below_pos = BlockPos { x: origin.x, y: origin.y - 1, z: origin.z };
        if let Some(state) = below_provider.get_state_id(grid, tags, random, below_pos) {
            grid.set_id_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
            trunk_positions.push(below_pos);
        }
    }

    let mut placed_any = false;
    const STEP: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)]; // NORTH, EAST, SOUTH, WEST

    for height_pos in 0..tree_height {
        let current_height = origin.y + height_pos;
        let pos = BlockPos { x: origin.x, y: current_height, z: origin.z };
        let placed_here = place_upwards_branching_valid(grid, tags, can_grow_through, pos.x, pos.y, pos.z)
            && {
                if let Some(state) = trunk_provider.get_state_id(grid, tags, random, pos) {
                    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
                    placed_any = true;
                    trunk_positions.push(pos);
                    true
                } else {
                    false
                }
            };

        if placed_here && height_pos < tree_height - 1 && random.next_float() < place_branch_per_log_probability {
            let branch_dir = STEP[random.next_int_bounded(4) as usize];
            let branch_len = extra_branch_length.sample(random);
            let branch_pos = (branch_len - extra_branch_length.sample(random) - 1).max(0);
            let branch_steps = extra_branch_steps.sample(random);
            place_mangrove_branch(
                random,
                origin,
                tree_height,
                grid,
                tags,
                trunk_provider,
                can_grow_through,
                current_height,
                branch_dir,
                branch_pos,
                branch_steps,
                &mut placed_any,
                attachments,
                trunk_positions,
            );
        }

        if height_pos == tree_height - 1 {
            attachments.push(Attachment {
                pos: BlockPos { x: origin.x, y: current_height + 1, z: origin.z },
                radius_offset: 0,
                double_trunk: false,
            });
        }
    }

    placed_any
}

/// `UpwardsBranchingTrunkPlacer.placeBranch`. `log_x`/`log_z` walk away from
/// the trunk one step per iteration (never reset to the trunk column); the
/// FIRST iteration (`branch_placement_index == branch_pos`, which can be `0`)
/// is deliberately skipped by the real Java's own `if (branchPlacementIndex
/// >= 1)` guard, so a `branch_pos` of `0` places its first REAL log one step
/// further out than the loop's own starting index, not at the trunk itself.
#[allow(clippy::too_many_arguments)]
fn place_mangrove_branch<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_provider: &BlockStateProvider,
    can_grow_through: Tag,
    current_height: i32,
    branch_dir: (i32, i32),
    branch_pos: i32,
    mut branch_steps: i32,
    placed_any: &mut bool,
    attachments: &mut Vec<Attachment>,
    trunk_positions: &mut Vec<BlockPos>,
) {
    let mut height_along_branch = current_height + branch_pos;
    let mut log_x = origin.x;
    let mut log_z = origin.z;
    let mut branch_placement_index = branch_pos;

    while branch_placement_index < tree_height && branch_steps > 0 {
        if branch_placement_index >= 1 {
            let placement_height = current_height + branch_placement_index;
            log_x += branch_dir.0;
            log_z += branch_dir.1;
            height_along_branch = placement_height;
            let pos = BlockPos { x: log_x, y: placement_height, z: log_z };
            if place_upwards_branching_valid(grid, tags, can_grow_through, pos.x, pos.y, pos.z) {
                if let Some(state) = trunk_provider.get_state_id(grid, tags, random, pos) {
                    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
                    *placed_any = true;
                    trunk_positions.push(pos);
                    height_along_branch += 1;
                }
            }
            attachments.push(Attachment { pos, radius_offset: 0, double_trunk: false });
        }
        branch_placement_index += 1;
        branch_steps -= 1;
    }

    if height_along_branch - current_height > 1 {
        let foliage_pos = BlockPos { x: log_x, y: height_along_branch, z: log_z };
        attachments.push(Attachment { pos: foliage_pos, radius_offset: 0, double_trunk: false });
        attachments.push(Attachment {
            pos: BlockPos { x: foliage_pos.x, y: foliage_pos.y - 2, z: foliage_pos.z },
            radius_offset: 0,
            double_trunk: false,
        });
    }
}

/// `UpwardsBranchingTrunkPlacer.validTreePos` — [`valid_tree_pos`] OR the
/// species' own `can_grow_through` tag.
fn place_upwards_branching_valid(
    grid: &VegGrid,
    tags: &VegTags,
    can_grow_through: Tag,
    x: i32,
    y: i32,
    z: i32,
) -> bool {
    valid_tree_pos(grid, tags, x, y, z) || tags.has(grid.interner(), can_grow_through, grid.get_id(x, y, z))
}

/// `net.minecraft.world.level.levelgen.feature.rootplacers.AboveRootPlacement`
/// — `MangroveRootPlacer`'s optional extra block dropped above a root, e.g.
/// moss carpet.
#[derive(Clone, Debug)]
pub(super) struct AboveRootPlacementCfg {
    pub(super) chance: f32,
    pub(super) provider: BlockStateProvider,
}

/// `net.minecraft.world.level.levelgen.feature.rootplacers.RootPlacer` (the
/// `MangroveRootPlacer` subclass — no other
/// vanilla `RootPlacer` exists as of 26.2, so this is a one-variant enum for
/// the same reason [`TrunkPlacerCfg`]/[`FoliagePlacerCfg`] started as
/// one-variant enums originally). [`super::place::place_roots`] is the port
/// of `placeRoots`/`simulateRoots`/`potentialRootPositions`/`placeRoot`.
#[derive(Clone, Debug)]
pub(super) enum RootPlacerCfg {
    Mangrove {
        trunk_offset_y: IntProvider,
        root_provider: BlockStateProvider,
        above_root_placement: Option<AboveRootPlacementCfg>,
        can_grow_through: Tag,
        muddy_roots_in: Vec<String>,
        muddy_roots_provider: BlockStateProvider,
        max_root_width: i32,
        max_root_length: i32,
        random_skew_chance: f32,
    },
}

impl RootPlacerCfg {
    pub(super) fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "mangrove_root_placer" => {
                let trunk_offset_y = try_parse_int_provider(&v["trunk_offset_y"])?;
                let root_provider = BlockStateProvider::try_parse(&v["root_provider"])?;
                let above_root_placement = match v.get("above_root_placement") {
                    Some(a) if !a.is_null() => Some(AboveRootPlacementCfg {
                        chance: a["above_root_placement_chance"].as_f64()? as f32,
                        provider: BlockStateProvider::try_parse(&a["above_root_provider"])?,
                    }),
                    _ => None,
                };
                let mrp = &v["mangrove_root_placement"];
                let muddy_roots_in = super::config::parse_id_list(&mrp["muddy_roots_in"]);
                let muddy_roots_provider = BlockStateProvider::try_parse(&mrp["muddy_roots_provider"])?;
                let max_root_width = mrp["max_root_width"].as_i64()? as i32;
                let max_root_length = mrp["max_root_length"].as_i64()? as i32;
                let random_skew_chance = mrp["random_skew_chance"].as_f64()? as f32;
                Some(Self::Mangrove {
                    trunk_offset_y,
                    root_provider,
                    above_root_placement,
                    can_grow_through: Tag::MangroveRootsCanGrowThrough,
                    muddy_roots_in,
                    muddy_roots_provider,
                    max_root_width,
                    max_root_length,
                    random_skew_chance,
                })
            }
            _ => None,
        }
    }

    /// `RootPlacer.getTrunkOrigin` — `origin.above(trunkOffsetY.sample(random))`.
    pub(super) fn get_trunk_origin<R: RandomSource>(&self, origin: BlockPos, random: &mut R) -> BlockPos {
        match self {
            Self::Mangrove { trunk_offset_y, .. } => {
                BlockPos { x: origin.x, y: origin.y + trunk_offset_y.sample(random), z: origin.z }
            }
        }
    }
}

/// `RootPlacer.canPlaceRoot`/`MangroveRootPlacer.canPlaceRoot` —
/// [`valid_tree_pos`] OR the species' `can_grow_through` tag.
pub(super) fn can_place_root(grid: &VegGrid, tags: &VegTags, can_grow_through: Tag, pos: BlockPos) -> bool {
    valid_tree_pos(grid, tags, pos.x, pos.y, pos.z)
        || tags.has(grid.interner(), can_grow_through, grid.get_id(pos.x, pos.y, pos.z))
}

/// `MangroveRootPlacer.potentialRootPositions` — up to two candidate
/// positions for the next root segment, drawn from `pos`'s manhattan
/// distance to `root_origin` and, in the two RNG-bearing branches, real
/// draws. Order matches Java's `List.of(...)` construction exactly (`below`
/// first where both are returned).
fn potential_root_positions<R: RandomSource>(
    pos: BlockPos,
    prev_dir: (i32, i32),
    random: &mut R,
    root_origin: BlockPos,
    max_root_width: i32,
    random_skew_chance: f32,
    out: &mut Vec<BlockPos>,
) {
    let below = BlockPos { x: pos.x, y: pos.y - 1, z: pos.z };
    let next_to = BlockPos { x: pos.x + prev_dir.0, y: pos.y, z: pos.z + prev_dir.1 };
    let width = (pos.x - root_origin.x).abs() + (pos.y - root_origin.y).abs() + (pos.z - root_origin.z).abs();
    if width > max_root_width - 3 && width <= max_root_width {
        if random.next_float() < random_skew_chance {
            out.push(below);
            out.push(BlockPos { x: next_to.x, y: next_to.y - 1, z: next_to.z });
        } else {
            out.push(below);
        }
    } else if width > max_root_width {
        out.push(below);
    } else if random.next_float() < random_skew_chance {
        out.push(below);
    } else if random.next_bool() {
        out.push(next_to);
    } else {
        out.push(below);
    }
}

/// `MangroveRootPlacer.simulateRoots` — recurses along one direction until
/// either it runs out of room (`canPlaceRoot` fails for every candidate at a
/// layer — a normal, successful stop) or `layer` reaches `max_root_length`
/// (`layer != maxRootLength` going false), in which case the WHOLE root
/// placement is abandoned — see [`super::place::place_roots`]'s own doc for
/// why a false here propagates all the way out and cancels the tree.
#[allow(clippy::too_many_arguments)]
pub(super) fn simulate_roots<R: RandomSource>(
    random: &mut R,
    root_pos: BlockPos,
    dir: (i32, i32),
    root_origin: BlockPos,
    root_positions: &mut Vec<BlockPos>,
    layer: i32,
    grid: &VegGrid,
    tags: &VegTags,
    can_grow_through: Tag,
    max_root_length: i32,
    max_root_width: i32,
    random_skew_chance: f32,
) -> bool {
    if layer != max_root_length && root_positions.len() as i32 <= max_root_length {
        let mut candidates = Vec::with_capacity(2);
        potential_root_positions(root_pos, dir, random, root_origin, max_root_width, random_skew_chance, &mut candidates);
        for pos in candidates {
            if can_place_root(grid, tags, can_grow_through, pos) {
                root_positions.push(pos);
                if !simulate_roots(
                    random, pos, dir, root_origin, root_positions, layer + 1, grid, tags, can_grow_through,
                    max_root_length, max_root_width, random_skew_chance,
                ) {
                    return false;
                }
            }
        }
        true
    } else {
        false
    }
}

/// `TreeFeature.updateLeaves` — the real post-processing pass vanilla runs
/// after a tree's trunk, foliage AND decorators have all been placed: a
/// multi-source BFS from every position in `trunk_positions` (bucket 0),
/// lowering every reachable `distance`-carrying block's `distance` property
/// to the true shortest distance-to-a-log, capped at 7 (never written past
/// that cap, matching `LeavesBlock.DECAY_DISTANCE`). This is why every
/// configured leaves state's own JSON-literal `distance` (always `7`, the
/// "fresh, undecayed" default) is not what real vanilla ever actually
/// serves near a trunk — before this function existed, this engine placed
/// every leaf at the JSON's literal `distance=7` and never corrected it, a
/// real, measured mismatch found by this change's savanna oracle fixtures
/// (real oak/acacia canopies are NOT reachable at plains' ~5%-per-chunk tree
/// rate with the two originally-committed fixtures, which is why this
/// gap was invisible until now — see this module's own parity test's doc
/// comment "A real bug in the oracle itself" for the reason trees were
/// never actually exercised before).
///
/// **Not a literal line-for-line port of the bucket/queue mechanics** — the
/// real Java keeps `toCheck`'s buckets as `Set`s and only guards re-adding
/// a position via a separately-tracked `DiscreteVoxelShape` "filled" bitset
/// checked at *dequeue* time (`shape.fill`)/*enqueue* time
/// (`shape.isFull`). A first attempt at translating that literally with
/// per-bucket `VecDeque`s and no cross-bucket dedup **hung indefinitely**:
/// a log's neighbour (a leaf) enqueues the log's own position back into
/// bucket 0 every time it is visited (the log always answers distance `0`,
/// so `min(smallest+1, 0)` is always `0`), and with no de-duplication nothing
/// ever stops that log from being re-popped and re-expanding the exact same
/// leaf forever. This function instead tracks one `visited: HashSet` and
/// marks a position the moment it is *enqueued* (not when it is later
/// popped) — a standard, well-known equivalent formulation of a uniform
/// (all-edge-weight-`1`) multi-source BFS via a bucket queue: the first
/// discovery of any position, under a discipline that always drains the
/// current-nearest bucket completely before advancing, **is** its true
/// shortest distance, so marking on first discovery cannot produce a
/// different final value than marking on completion — it only prevents the
/// redundant re-enqueues that made the literal port hang. This changes
/// nothing about *which* `distance` value ultimately gets written to any
/// cell, only how many times an already-settled cell gets looked at again.
/// No RNG is consumed anywhere in this function (a pure grid post-process),
/// so none of this affects the decoration RNG stream either way.
///
/// `#minecraft:prevents_nearby_leaf_decay` is, in the real registry, defined
/// as exactly `["#minecraft:logs"]` (`prevents_nearby_leaf_decay.json`) —
/// not an approximation, so this reuses [`VegTags::logs`] rather than
/// resolving a second, redundant tag.
///
/// **`bbox` is load-bearing, not a perf bound.** Real
/// `TreeFeature.place`/`updateLeaves` scopes its own BFS to
/// `BoundingBox.encapsulatingPositions(trunks ∪ foliage ∪ decorations ∪
/// roots)` — the bounding box of THIS ONE TREE's own placed blocks, not the
/// whole world — and gates BOTH the write and the neighbour-expansion step
/// on `bounds.isInside(pos)`. A first version of this port had no such
/// bound at all (any in-grid position was fair game), and measured wrong
/// against real savanna oracle fixtures: it found a *closer* neighbouring
/// tree's log through gaps between two adjacent canopies, giving every
/// affected leaf a lower `distance` than vanilla's own bbox-scoped BFS ever
/// would (vanilla's version, confined to one tree's own extent, simply
/// cannot see a different tree's logs at all, no matter how close). `bbox`
/// is `(min_x, min_y, min_z, max_x, max_y, max_z)`, inclusive, computed by
/// the caller from exactly the positions this one [`place_tree`] call wrote
/// (see that function's own call site for how).
pub(super) fn update_leaf_distances(
    grid: &mut VegGrid,
    tags: &VegTags,
    trunk_positions: &[BlockPos],
    bbox: (i32, i32, i32, i32, i32, i32),
) {
    const MAX_DISTANCE: i32 = 7;
    const NEIGHBOR_OFFSETS: [(i32, i32, i32); 6] =
        [(0, -1, 0), (0, 1, 0), (-1, 0, 0), (1, 0, 0), (0, 0, -1), (0, 0, 1)];
    let (min_x, min_y, min_z, max_x, max_y, max_z) = bbox;
    let inside = |x: i32, y: i32, z: i32| {
        (min_x..=max_x).contains(&x) && (min_y..=max_y).contains(&y) && (min_z..=max_z).contains(&z)
    };

    // Unit 8: the bucket queue and the visited set are reused thread-local
    // scratch, not eight fresh allocations per tree. `BFS.take()` (rather than a
    // borrow held across the body) keeps a hypothetical nested call correct — it
    // would get fresh, allocating buffers instead of a `RefCell` panic.
    let mut scratch = BFS.take();
    let Bfs { buckets, visited } = &mut scratch;
    buckets.resize_with(MAX_DISTANCE as usize, VecDeque::new);
    for bucket in buckets.iter_mut() {
        bucket.clear();
    }
    visited.clear();
    // Every trunk position is, by construction, inside `bbox` (the caller
    // derives `bbox` to encapsulate them) — matching real vanilla, where
    // `bounds` is built FROM `trunks`, so a log is trivially always its own
    // bbox member. No `inside` check needed here.
    for p in trunk_positions {
        let key = (p.x, p.y, p.z);
        if visited.insert(key) {
            buckets[0].push_back(key);
        }
    }

    let mut smallest: i32 = 0;
    loop {
        loop {
            if smallest >= MAX_DISTANCE {
                BFS.set(scratch);
                return;
            }
            let Some((x, y, z)) = buckets[smallest as usize].pop_front() else {
                break;
            };
            if smallest != 0 {
                // Unit 8: the `distance=N` rewrite is a memoised id -> id lookup.
                // It was `grid.get(..).to_string()` plus a `replace_range` plus a
                // re-intern, once per visited leaf — one of the three sites
                // `docs/worldgen-state-interning.md` names as this unit's residual.
                let id = grid.get_id(x, y, z);
                let new_state = tags.rewrite(
                    grid.interner(),
                    id,
                    Rewrite::Distance(u8::try_from(smallest).unwrap_or(0)),
                );
                if let Some(new_state) = new_state {
                    grid.set_id_if_in_bounds(x, y, z, new_state);
                }
            }
            for (dx, dy, dz) in NEIGHBOR_OFFSETS {
                let (nx, ny, nz) = (x + dx, y + dy, z + dz);
                let neighbor_key = (nx, ny, nz);
                if visited.contains(&neighbor_key) {
                    continue;
                }
                // The real `bounds.isInside(neighborPos)` gate — see this
                // function's own doc comment on why this must be the
                // tree's own bbox, not the grid's whole footprint.
                if !inside(nx, ny, nz) {
                    continue;
                }
                let neighbor = grid.get_id(nx, ny, nz);
                let current_distance = if tags.has(grid.interner(), Tag::Logs, neighbor) {
                    Some(0)
                } else {
                    tags.distance_of(grid.interner(), neighbor)
                };
                if let Some(current_distance) = current_distance {
                    let new_distance = (smallest + 1).min(current_distance);
                    if new_distance < MAX_DISTANCE {
                        visited.insert(neighbor_key);
                        buckets[new_distance as usize].push_back(neighbor_key);
                        smallest = smallest.min(new_distance);
                    }
                }
            }
        }
        smallest += 1;
    }
}

/// [`update_leaf_distances`]' reusable bucket queue and visited set.
struct Bfs {
    buckets: Vec<VecDeque<(i32, i32, i32)>>,
    /// [`FastSet`], not the default hasher — the third of the vegetation maps U17
    /// measured at 0.8% of all worldgen CPU and left for this file's owner.
    ///
    /// Order-safe, and the argument is stronger here than "never iterated": the BFS
    /// **traversal** order comes entirely from `buckets`, and this set only ever
    /// answers membership (`clear`, `insert`, `contains` — no `iter`, no `drain`).
    /// So the leaf `distance` values this function assigns cannot depend on the
    /// hasher, which is what matters, because those values reach the wire.
    visited: FastSet<(i32, i32, i32)>,
}

impl Default for Bfs {
    fn default() -> Self {
        Self {
            buckets: Vec::new(),
            visited: FastSet::default(),
        }
    }
}

thread_local! {
    /// One tree's BFS scratch, reused across trees. Not `const`-initialised — the
    /// `Vec` would be, but `HashSet::with_hasher` is only usable here through
    /// `Default`, so the first touch on a thread allocates. That is warmup, not
    /// steady state, and is why the acceptance gate measures a *second* pass rather
    /// than the first.
    static BFS: RefCell<Bfs> = RefCell::new(Bfs::default());
}

// `distance_property` and `set_distance_property` used to live here, as `&str ->
// Option<i32>` and `&str -> Option<String>`. Unit 8 moved both into
// [`super::ids`] — `parse_distance` and `rewrite_property` — because the
// question is now asked of a `StateId` and answered from a table filled once per
// interner, and keeping a second string implementation beside it would be a
// definition free to drift from the one the table is built from. The `&str`
// bodies survive verbatim inside those two functions; nothing about the property
// syntax handling changed.

/// `net.minecraft.world.level.levelgen.feature.foliageplacers.FoliagePlacer`
/// (the `Blob`/`Spruce`/`Pine`/`Acacia` subset — see module doc's "Named
/// per-branch gaps" for why `Pine` is here despite not being one of the
/// three originally-named species; `Acacia` is the savanna/acacia increment's addition, paired
/// with [`TrunkPlacerCfg::Forking`]).
#[derive(Clone, Debug)]
pub enum FoliagePlacerCfg {
    Blob {
        height: i32,
        radius: IntProvider,
        offset: IntProvider,
    },
    Spruce {
        radius: IntProvider,
        offset: IntProvider,
        trunk_height: IntProvider,
    },
    Pine {
        height: IntProvider,
        radius: IntProvider,
        offset: IntProvider,
    },
    /// `AcaciaFoliagePlacer` — acacia's real foliage. Its
    /// `foliageHeight` override always returns the constant `0`, drawing no
    /// RNG at all (unlike `Blob`'s config-constant `height` field or
    /// `Pine`'s sampled one) — see [`Self::foliage_height`]'s own arm.
    Acacia {
        radius: IntProvider,
        offset: IntProvider,
    },
    /// `DarkOakFoliagePlacer` — dark oak's real foliage, paired
    /// with [`TrunkPlacerCfg::DarkOak`] (and shared with pale oak). Rows are
    /// drawn relative to each [`Attachment`] with radii that depend on
    /// `double_trunk` (`leafRadius + 2/-1`, `leafRadius + 3/0`,
    /// `leafRadius + 2/1`, plus a `nextBoolean`-gated `leafRadius/2` row for
    /// the primary 2×2-trunk attachment only), and the skip logic overrides
    /// the signed wrapper too — see [`Self::should_skip_location_signed`].
    /// `foliageHeight` is the constant `4`, no RNG.
    DarkOak {
        radius: IntProvider,
        offset: IntProvider,
    },
    /// `BushFoliagePlacer` — jungle_bush's real foliage: a
    /// `BlobFoliagePlacer` subclass (shares its `height` field, parsed the
    /// same way) that overrides both `createFoliage` (a different per-row
    /// radius formula, and no `/2` term) and `shouldSkipLocation` (an
    /// unconditional corner coin flip, not `Blob`'s `coin || y == 0`). See
    /// [`Self::create_foliage`]'s own `Bush` arm and
    /// [`Self::should_skip_location`]'s own `Bush` arm.
    Bush {
        height: i32,
        radius: IntProvider,
        offset: IntProvider,
    },
    /// `MegaJungleFoliagePlacer` — mega jungle's real foliage,
    /// paired with [`TrunkPlacerCfg::MegaJungle`]. Registered in vanilla as
    /// `"jungle_foliage_placer"` (not `"mega_jungle_foliage_placer"` — see
    /// `FoliagePlacerType.MEGA_JUNGLE_FOLIAGE_PLACER`'s own registration
    /// name). Carries its own `height` field like `Bush` above.
    MegaJungle {
        height: i32,
        radius: IntProvider,
        offset: IntProvider,
    },
    /// `MegaPineFoliagePlacer` — the redwood/giant-spruce foliage (added
    /// with the savanna/acacia increment), paired with [`TrunkPlacerCfg::Giant`] directly (mega_spruce/
    /// mega_pine use `giant_trunk_placer`, not `MegaJungleTrunkPlacer`).
    /// `crown_height` replaces the other placers' constant/derived
    /// `foliage_height` with its own sampled `IntProvider`.
    MegaPine {
        crown_height: IntProvider,
        radius: IntProvider,
        offset: IntProvider,
    },
    /// `FancyFoliagePlacer` — oak's `fancy_oak_*` foliage,
    /// paired with [`TrunkPlacerCfg::Fancy`]. A `BlobFoliagePlacer`
    /// subclass sharing its `height` field and parse shape (like
    /// [`Self::Bush`]/[`Self::MegaJungle`] above) but overriding both
    /// `createFoliage` (a widened-middle descending-row shape, no RNG in the
    /// radius formula itself) and `shouldSkipLocation` (a pure `(dx+0.5,
    /// dz+0.5)` distance test, no RNG draw — unlike `Blob`'s corner coin
    /// flip).
    Fancy {
        height: i32,
        radius: IntProvider,
        offset: IntProvider,
    },
    /// `CherryFoliagePlacer` — cherry's real foliage, paired
    /// with [`TrunkPlacerCfg::Cherry`]. `height` is a genuinely sampled
    /// `IntProvider` (unlike `Blob`/`Bush`/`MegaJungle`/`Fancy`'s constant
    /// literal). See [`Self::create_foliage`]'s own `Cherry` arm for the
    /// row layout and [`Self::should_skip_location`]'s own arm for the two
    /// hole-chance rolls.
    Cherry {
        radius: IntProvider,
        offset: IntProvider,
        height: IntProvider,
        wide_bottom_layer_hole_chance: f32,
        corner_hole_chance: f32,
        hanging_leaves_chance: f32,
        hanging_leaves_extension_chance: f32,
    },
    /// `RandomSpreadFoliagePlacer` — mangrove's real foliage,
    /// paired with [`TrunkPlacerCfg::UpwardsBranching`]. The only placer in
    /// this module with no `shouldSkipLocation`/row structure at all: it
    /// throws `leaf_placement_attempts` independent darts inside a box
    /// `radius × 2` wide and `foliage_height × 2` tall, each landing wherever
    /// two independent `nextInt(bound) - nextInt(bound)` draws put it (a
    /// triangular, not uniform, distribution — see [`Self::create_foliage`]'s
    /// own `RandomSpread` arm).
    RandomSpread {
        radius: IntProvider,
        offset: IntProvider,
        height: IntProvider,
        leaf_placement_attempts: i32,
    },
}

impl FoliagePlacerCfg {
pub(super)     fn try_parse(v: &Value) -> Option<Self> {
        let ty = v["type"].as_str()?;
        let radius = try_parse_int_provider(&v["radius"])?;
        let offset = try_parse_int_provider(&v["offset"])?;
        match ty.strip_prefix("minecraft:").unwrap_or(ty) {
            "blob_foliage_placer" => Some(FoliagePlacerCfg::Blob {
                height: v["height"].as_i64()? as i32,
                radius,
                offset,
            }),
            "spruce_foliage_placer" => Some(FoliagePlacerCfg::Spruce {
                radius,
                offset,
                trunk_height: try_parse_int_provider(&v["trunk_height"])?,
            }),
            "pine_foliage_placer" => Some(FoliagePlacerCfg::Pine {
                height: try_parse_int_provider(&v["height"])?,
                radius,
                offset,
            }),
            "acacia_foliage_placer" => Some(FoliagePlacerCfg::Acacia { radius, offset }),
            "dark_oak_foliage_placer" => Some(FoliagePlacerCfg::DarkOak { radius, offset }),
            "bush_foliage_placer" => Some(FoliagePlacerCfg::Bush {
                height: v["height"].as_i64()? as i32,
                radius,
                offset,
            }),
            "jungle_foliage_placer" => Some(FoliagePlacerCfg::MegaJungle {
                height: v["height"].as_i64()? as i32,
                radius,
                offset,
            }),
            "mega_pine_foliage_placer" => Some(FoliagePlacerCfg::MegaPine {
                crown_height: try_parse_int_provider(&v["crown_height"])?,
                radius,
                offset,
            }),
            "fancy_foliage_placer" => Some(FoliagePlacerCfg::Fancy {
                height: v["height"].as_i64()? as i32,
                radius,
                offset,
            }),
            "cherry_foliage_placer" => Some(FoliagePlacerCfg::Cherry {
                radius,
                offset,
                height: try_parse_int_provider(&v["height"])?,
                wide_bottom_layer_hole_chance: v["wide_bottom_layer_hole_chance"].as_f64()? as f32,
                corner_hole_chance: v["corner_hole_chance"].as_f64()? as f32,
                hanging_leaves_chance: v["hanging_leaves_chance"].as_f64()? as f32,
                hanging_leaves_extension_chance: v["hanging_leaves_extension_chance"].as_f64()? as f32,
            }),
            "random_spread_foliage_placer" => Some(FoliagePlacerCfg::RandomSpread {
                radius,
                offset,
                height: try_parse_int_provider(&v["foliage_height"])?,
                leaf_placement_attempts: v["leaf_placement_attempts"].as_i64()? as i32,
            }),
            _ => None,
        }
    }

pub(super)     fn foliage_height<R: RandomSource>(&self, random: &mut R, tree_height: i32) -> i32 {
        match self {
            FoliagePlacerCfg::Blob { height, .. } => *height,
            FoliagePlacerCfg::Spruce { trunk_height, .. } => {
                (tree_height - trunk_height.sample(random)).max(4)
            }
            FoliagePlacerCfg::Pine { height, .. } => height.sample(random),
            // `AcaciaFoliagePlacer.foliageHeight` ignores every one of its
            // own arguments and returns the constant `0` — no RNG draw.
            FoliagePlacerCfg::Acacia { .. } => 0,
            // `DarkOakFoliagePlacer.foliageHeight` returns the constant `4`
            // — no RNG draw.
            FoliagePlacerCfg::DarkOak { .. } => 4,
            // `BushFoliagePlacer` doesn't override `foliageHeight` — it
            // inherits `BlobFoliagePlacer`'s own constant `height` field, no
            // RNG draw, same shape as `Blob` above.
            FoliagePlacerCfg::Bush { height, .. } => *height,
            // `MegaJungleFoliagePlacer.foliageHeight` returns its own
            // constant `height` field — no RNG draw.
            FoliagePlacerCfg::MegaJungle { height, .. } => *height,
            // `MegaPineFoliagePlacer.foliageHeight` is the one override in
            // this module that samples an `IntProvider` here rather than
            // returning a config-literal constant.
            FoliagePlacerCfg::MegaPine { crown_height, .. } => crown_height.sample(random),
            // `FancyFoliagePlacer` doesn't override `foliageHeight` either —
            // inherits `BlobFoliagePlacer`'s own constant `height` field,
            // same shape as `Blob`/`Bush`/`MegaJungle` above.
            FoliagePlacerCfg::Fancy { height, .. } => *height,
            // `CherryFoliagePlacer`/`RandomSpreadFoliagePlacer.foliageHeight` —
            // both sample a real `IntProvider` field (no constant, unlike
            // `Blob`/`Bush`/`MegaJungle`/`Fancy` above).
            FoliagePlacerCfg::Cherry { height, .. } | FoliagePlacerCfg::RandomSpread { height, .. } => {
                height.sample(random)
            }
        }
    }

pub(super)     fn foliage_radius<R: RandomSource>(&self, random: &mut R, trunk_len: i32) -> i32 {
        match self {
            FoliagePlacerCfg::Blob { radius, .. }
            | FoliagePlacerCfg::Spruce { radius, .. }
            | FoliagePlacerCfg::Acacia { radius, .. }
            | FoliagePlacerCfg::DarkOak { radius, .. }
            | FoliagePlacerCfg::Bush { radius, .. }
            | FoliagePlacerCfg::MegaJungle { radius, .. }
            | FoliagePlacerCfg::MegaPine { radius, .. }
            | FoliagePlacerCfg::Fancy { radius, .. }
            // Neither `CherryFoliagePlacer` nor `RandomSpreadFoliagePlacer`
            // overrides `foliageRadius` — both inherit `FoliagePlacer`'s own
            // base `this.radius.sample(random)`.
            | FoliagePlacerCfg::Cherry { radius, .. }
            | FoliagePlacerCfg::RandomSpread { radius, .. } => radius.sample(random),
            FoliagePlacerCfg::Pine { radius, .. } => {
                radius.sample(random) + random.next_int_bounded(trunk_len.max(0) + 1)
            }
        }
    }

pub(super)     fn sample_offset<R: RandomSource>(&self, random: &mut R) -> i32 {
        match self {
            FoliagePlacerCfg::Blob { offset, .. }
            | FoliagePlacerCfg::Spruce { offset, .. }
            | FoliagePlacerCfg::Pine { offset, .. }
            | FoliagePlacerCfg::Acacia { offset, .. }
            | FoliagePlacerCfg::DarkOak { offset, .. }
            | FoliagePlacerCfg::Bush { offset, .. }
            | FoliagePlacerCfg::MegaJungle { offset, .. }
            | FoliagePlacerCfg::MegaPine { offset, .. }
            | FoliagePlacerCfg::Fancy { offset, .. }
            | FoliagePlacerCfg::Cherry { offset, .. }
            | FoliagePlacerCfg::RandomSpread { offset, .. } => offset.sample(random),
        }
    }

    /// `FoliagePlacer.shouldSkipLocation` — the plain (already-abs'd,
    /// `doubleTrunk`-free) skip predicate, the leaf of
    /// [`Self::should_skip_location_signed`]'s default path for every placer
    /// except [`FoliagePlacerCfg::DarkOak`] (which overrides the signed
    /// wrapper *and* the inner predicate, so its own arm here is unreachable
    /// by construction — kept as `false` rather than `unreachable!()` to
    /// honour this module's degrade-don't-panic rule).
    fn should_skip_location<R: RandomSource>(
        &self,
        random: &mut R,
        dx: i32,
        y: i32,
        dz: i32,
        current_radius: i32,
    ) -> bool {
        match self {
            FoliagePlacerCfg::Blob { .. } => {
                if dx == current_radius && dz == current_radius {
                    // Always drawn when at the corner, regardless of `y` —
                    // `nextInt(2)` is the *left* operand of `||`, and Java
                    // never short-circuits away from evaluating it first.
                    let coin = random.next_int_bounded(2) == 0;
                    coin || y == 0
                } else {
                    false
                }
            }
            FoliagePlacerCfg::Spruce { .. } | FoliagePlacerCfg::Pine { .. } => {
                dx == current_radius && dz == current_radius && current_radius > 0
            }
            // `AcaciaFoliagePlacer.shouldSkipLocation` — pure geometry, no
            // RNG draw (unlike `Blob`'s corner coin flip above).
            FoliagePlacerCfg::Acacia { .. } => {
                if y == 0 {
                    (dx > 1 || dz > 1) && dx != 0 && dz != 0
                } else {
                    dx == current_radius && dz == current_radius && current_radius > 0
                }
            }
            // Unreachable — [`Self::should_skip_location_signed`] handles
            // DarkOak entirely (see that method's own doc).
            FoliagePlacerCfg::DarkOak { .. } => false,
            // `BushFoliagePlacer.shouldSkipLocation` — an UNCONDITIONAL
            // corner coin flip (unlike `Blob`'s `coin || y == 0`, this has no
            // `y`-based override — the draw's result alone decides).
            FoliagePlacerCfg::Bush { .. } => {
                dx == current_radius && dz == current_radius && random.next_int_bounded(2) == 0
            }
            // `MegaJungleFoliagePlacer.shouldSkipLocation` and
            // `MegaPineFoliagePlacer.shouldSkipLocation` are textually
            // identical in real vanilla — pure geometry, no RNG draw.
            FoliagePlacerCfg::MegaJungle { .. } | FoliagePlacerCfg::MegaPine { .. } => {
                dx + dz >= 7 || dx * dx + dz * dz > current_radius * current_radius
            }
            // `FancyFoliagePlacer.shouldSkipLocation` —
            // `Mth.square(dx+0.5F) + Mth.square(dz+0.5F) > currentRadius^2`,
            // comparing a float sum against an int product widened to
            // float. Pure geometry, no RNG draw.
            FoliagePlacerCfg::Fancy { .. } => {
                let dxf = dx as f32 + 0.5;
                let dzf = dz as f32 + 0.5;
                let rr = (current_radius * current_radius) as f32;
                dxf * dxf + dzf * dzf > rr
            }
            // `CherryFoliagePlacer.shouldSkipLocation`. `y == -1`'s two-edge
            // hole roll is checked and drawn FIRST and short-circuits the
            // whole call if it fires — the corner/wide-layer roll below is
            // never reached in that case, matching Java's `if { return
            // true; }` early-out exactly (not merely `||`d together with it).
            FoliagePlacerCfg::Cherry { wide_bottom_layer_hole_chance, corner_hole_chance, .. } => {
                if y == -1
                    && (dx == current_radius || dz == current_radius)
                    && random.next_float() < *wide_bottom_layer_hole_chance
                {
                    return true;
                }
                let corner = dx == current_radius && dz == current_radius;
                let wide_layer = current_radius > 2;
                if wide_layer {
                    corner || (dx + dz > current_radius * 2 - 2 && random.next_float() < *corner_hole_chance)
                } else {
                    corner && random.next_float() < *corner_hole_chance
                }
            }
            // `RandomSpreadFoliagePlacer.shouldSkipLocation` returns the
            // constant `false` — but this arm is genuinely unreachable in
            // practice, because [`Self::create_foliage`]'s own `RandomSpread`
            // arm never calls [`place_leaves_row`]/`should_skip_location_signed`
            // at all (`createFoliage` throws darts directly via
            // [`try_place_leaf`], matching real
            // `RandomSpreadFoliagePlacer.createFoliage`, which never calls
            // `placeLeavesRow` either).
            FoliagePlacerCfg::RandomSpread { .. } => false,
        }
    }

    /// `FoliagePlacer.shouldSkipLocationSigned` — the entry
    /// [`place_leaves_row`] always uses (vanilla's `placeLeavesRow` calls the
    /// signed wrapper, never the plain predicate). For every placer except
    /// [`FoliagePlacerCfg::DarkOak`] this is exactly the wrapper's default:
    /// `shouldSkipLocation(|dx|, |dz|)` — identical to what the callers
    /// previously passed to [`Self::should_skip_location`] directly, so no
    /// draw-count or result changes for oak/birch/spruce/pine/acacia.
    ///
    /// `DarkOakFoliagePlacer` overrides BOTH the signed wrapper and the inner
    /// predicate in real vanilla, so it gets its own arm. The wrapper
    /// override short-circuits to `true` (skip) only for the double-trunk
    /// `y == 0` row when `dx` AND `dz` are both at the row's extremes
    /// (`dx == -r || dx >= r` and the same for `dz` — the corner 2×2s of the
    /// widened row); everything else delegates to the default wrapper, which
    /// for a double trunk computes `min(|dx|, |dx - 1|)`/`min(|dz|, |dz - 1|)`
    /// (the distance to the nearer of the 2×2 trunk's two columns) before the
    /// inner predicate: `y == -1 && !doubleTrunk` skips the corners, and
    /// `y == 1` skips where `minDx + minDz > 2 * r - 2`. All pure geometry —
    /// `DarkOakFoliagePlacer` draws no RNG in its skip logic.
    fn should_skip_location_signed<R: RandomSource>(
        &self,
        random: &mut R,
        dx: i32,
        y: i32,
        dz: i32,
        current_radius: i32,
        double_trunk: bool,
    ) -> bool {
        match self {
            FoliagePlacerCfg::DarkOak { .. } => {
                // `DarkOakFoliagePlacer.shouldSkipLocationSigned`'s
                // short-circuit head: delegate to the superclass unless
                // `y == 0 && doubleTrunk && dx && dz` are all "at the edge".
                let delegate = y != 0
                    || !double_trunk
                    || (dx != -current_radius && dx < current_radius)
                    || (dz != -current_radius && dz < current_radius);
                if !delegate {
                    return true;
                }
                // `FoliagePlacer.shouldSkipLocationSigned`'s default (the
                // super call), then `DarkOakFoliagePlacer.shouldSkipLocation`.
                let (min_dx, min_dz) = if double_trunk {
                    (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs()))
                } else {
                    (dx.abs(), dz.abs())
                };
                if y == -1 && !double_trunk {
                    min_dx == current_radius && min_dz == current_radius
                } else if y == 1 {
                    min_dx + min_dz > current_radius * 2 - 2
                } else {
                    false
                }
            }
            _ => self.should_skip_location(random, dx.abs(), y, dz.abs(), current_radius),
        }
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
pub(super)     fn create_foliage<R: RandomSource>(
        &self,
        random: &mut R,
        attachment: BlockPos,
        foliage_height: i32,
        leaf_radius: i32,
        offset: i32,
        radius_offset: i32,
        double_trunk: bool,
        grid: &mut VegGrid,
        tags: &VegTags,
        provider: &BlockStateProvider,
        foliage_positions: &mut FastSet<(i32, i32, i32)>,
        placed_any: &mut bool,
    ) {
        match self {
            FoliagePlacerCfg::Blob { .. } => {
                // `yo / 2` in both Java and Rust truncates toward zero, so
                // no special-casing is needed for negative `yo` here.
                for yo in (offset - foliage_height..=offset).rev() {
                    let radius = (leaf_radius - 1 - yo / 2).max(0);
                    place_leaves_row(
                        random, attachment, radius, yo, grid, tags, self, provider, foliage_positions, placed_any, false,
                    );
                }
            }
            FoliagePlacerCfg::Spruce { .. } => {
                let mut current_radius = random.next_int_bounded(2);
                let mut max_radius = 1;
                let mut min_radius = 0;
                let mut yo = offset;
                while yo >= -foliage_height {
                    place_leaves_row(
                        random,
                        attachment,
                        current_radius,
                        yo,
                        grid,
                        tags,
                        self,
                        provider,
                        foliage_positions,
                        placed_any,
                        false,
                    );
                    if current_radius >= max_radius {
                        current_radius = min_radius;
                        min_radius = 1;
                        max_radius = (max_radius + 1).min(leaf_radius);
                    } else {
                        current_radius += 1;
                    }
                    yo -= 1;
                }
            }
            FoliagePlacerCfg::Pine { .. } => {
                let mut current_radius = 0;
                let lower = offset - foliage_height;
                let mut yo = offset;
                while yo >= lower {
                    place_leaves_row(
                        random,
                        attachment,
                        current_radius,
                        yo,
                        grid,
                        tags,
                        self,
                        provider,
                        foliage_positions,
                        placed_any,
                        false,
                    );
                    if current_radius >= 1 && yo == lower + 1 {
                        current_radius -= 1;
                    } else if current_radius < leaf_radius {
                        current_radius += 1;
                    }
                    yo -= 1;
                }
            }
            // `AcaciaFoliagePlacer.createFoliage` — exactly three explicit
            // `placeLeavesRow` calls (not a scanning loop like `Blob`/
            // `Spruce`/`Pine` above), at `y = -1 - foliageHeight`,
            // `-foliageHeight`, `0` — with `foliageHeight` always `0` (see
            // `Self::foliage_height`), that's `y = -1, 0, 0`. The two `y =
            // 0` rows use DIFFERENT radii (`leaf_radius - 1` then
            // `leaf_radius + radius_offset - 1`) and are both real, in that
            // exact order — the second simply overwrites part of what the
            // first already wrote, matching vanilla's own redundancy rather
            // than an engine bug.
            FoliagePlacerCfg::Acacia { .. } => {
                place_leaves_row(
                    random, attachment, leaf_radius + radius_offset, -1 - foliage_height, grid, tags, self, provider, foliage_positions, placed_any, false,
                );
                place_leaves_row(random, attachment, leaf_radius - 1, -foliage_height, grid, tags, self, provider, foliage_positions, placed_any, false);
                place_leaves_row(
                    random, attachment, leaf_radius + radius_offset - 1, 0, grid, tags, self, provider, foliage_positions, placed_any, false,
                );
            }
            // `DarkOakFoliagePlacer.createFoliage` — `pos =
            // foliageAttachment.pos().above(offset)`, then three rows whose
            // radii depend on `doubleTrunk` (`leafRadius + 2`, `+ 3`, `+ 2`
            // at `y = -1, 0, 1`), plus a `nextBoolean`-gated `leafRadius` row
            // at `y = 2` for the primary 2×2-trunk attachment only. The
            // non-double branches get two rows (`leafRadius + 2` at `-1`,
            // `leafRadius + 1` at `0`).
            FoliagePlacerCfg::DarkOak { .. } => {
                let pos = BlockPos {
                    x: attachment.x,
                    y: attachment.y + offset,
                    z: attachment.z,
                };
                if double_trunk {
                    place_leaves_row(random, pos, leaf_radius + 2, -1, grid, tags, self, provider, foliage_positions, placed_any, true);
                    place_leaves_row(random, pos, leaf_radius + 3, 0, grid, tags, self, provider, foliage_positions, placed_any, true);
                    place_leaves_row(random, pos, leaf_radius + 2, 1, grid, tags, self, provider, foliage_positions, placed_any, true);
                    if random.next_bool() {
                        place_leaves_row(random, pos, leaf_radius, 2, grid, tags, self, provider, foliage_positions, placed_any, true);
                    }
                } else {
                    place_leaves_row(random, pos, leaf_radius + 2, -1, grid, tags, self, provider, foliage_positions, placed_any, false);
                    place_leaves_row(random, pos, leaf_radius + 1, 0, grid, tags, self, provider, foliage_positions, placed_any, false);
                }
            }
            // `BushFoliagePlacer.createFoliage` — same descending-row shape
            // as `Blob`, but the radius formula has no `/2` term and adds
            // `radius_offset` (always `0` here — jungle_bush's only trunk
            // placer, `Straight`, never sets it nonzero, but it is threaded
            // through for fidelity with the real formula).
            FoliagePlacerCfg::Bush { .. } => {
                for yo in (offset - foliage_height..=offset).rev() {
                    let radius = leaf_radius + radius_offset - 1 - yo;
                    place_leaves_row(
                        random, attachment, radius, yo, grid, tags, self, provider, foliage_positions, placed_any, false,
                    );
                }
            }
            // `MegaJungleFoliagePlacer.createFoliage` — `leafHeight` draws
            // RNG only for a non-double-trunk (branch) attachment; the
            // primary (`double_trunk: true`) attachment uses the constant
            // `foliage_height` field instead, no draw. Rows descend from
            // `offset` to `offset - leafHeight` inclusive.
            FoliagePlacerCfg::MegaJungle { .. } => {
                let leaf_height = if double_trunk {
                    foliage_height
                } else {
                    1 + random.next_int_bounded(2)
                };
                let mut yo = offset;
                while yo >= offset - leaf_height {
                    let current_radius = leaf_radius + radius_offset + 1 - yo;
                    place_leaves_row(
                        random, attachment, current_radius, yo, grid, tags, self, provider, foliage_positions, placed_any, double_trunk,
                    );
                    yo -= 1;
                }
            }
            // `MegaPineFoliagePlacer.createFoliage` — the one placer in this
            // module whose rows are addressed by ABSOLUTE Y (via a shifted
            // row origin, `y = 0` every call) rather than a relative offset
            // passed to `place_leaves_row`'s own `y` parameter, and whose
            // radius is smoothed then "jagged" every other row. No RNG draw
            // anywhere in this arithmetic — `Mth.floor` and the `(yy & 1)`
            // parity check are pure. `yy % 2 == 0` is Rust's equivalent of
            // Java's `(yy & 1) == 0` for this equality-to-zero check: both
            // are zero exactly when `yy` is even, for either sign.
            FoliagePlacerCfg::MegaPine { .. } => {
                let mut prev_radius = 0;
                let start_yy = attachment.y - foliage_height + offset;
                let end_yy = attachment.y + offset;
                let mut yy = start_yy;
                while yy <= end_yy {
                    let yo = attachment.y - yy;
                    let smooth_radius = leaf_radius
                        + radius_offset
                        + lodestone_worldgen_core::math::floor(
                            f64::from((yo as f32 / foliage_height as f32) * 3.5_f32),
                        );
                    let jagged_radius = if yo > 0 && smooth_radius == prev_radius && yy % 2 == 0 {
                        smooth_radius + 1
                    } else {
                        smooth_radius
                    };
                    place_leaves_row(
                        random,
                        BlockPos { x: attachment.x, y: yy, z: attachment.z },
                        jagged_radius,
                        0,
                        grid,
                        tags,
                        self,
                        provider,
                        foliage_positions,
                        placed_any,
                        double_trunk,
                    );
                    prev_radius = smooth_radius;
                    yy += 1;
                }
            }
            // `FancyFoliagePlacer.createFoliage` — descends from `offset` to
            // `offset - foliageHeight` inclusive (same direction as
            // `Blob`/`Bush` above), widening the radius by 1 for every row
            // EXCEPT the very top (`yo == offset`) and very bottom
            // (`yo == offset - foliageHeight`) row. No RNG in the radius
            // formula itself.
            FoliagePlacerCfg::Fancy { .. } => {
                for yo in (offset - foliage_height..=offset).rev() {
                    let current_radius =
                        leaf_radius + if yo != offset && yo != offset - foliage_height { 1 } else { 0 };
                    place_leaves_row(
                        random, attachment, current_radius, yo, grid, tags, self, provider, foliage_positions, placed_any,
                        double_trunk,
                    );
                }
            }
            // `CherryFoliagePlacer.createFoliage` — `foliagePos =
            // foliageAttachment.pos().above(offset)`, `currentRadius =
            // leafRadius + radiusOffset - 1`, two fixed-radius rows at
            // `foliageHeight - 3`/`foliageHeight - 4`, then a full-radius
            // scan down to `y = 0`, then two rows that ALSO try to hang a
            // extra one or two leaves below themselves
            // ([`place_leaves_row_with_hanging_leaves_below`]) at `y = -1`
            // (radius `currentRadius`) and `y = -2` (radius `currentRadius -
            // 1`).
            FoliagePlacerCfg::Cherry { hanging_leaves_chance, hanging_leaves_extension_chance, .. } => {
                let foliage_pos = BlockPos { x: attachment.x, y: attachment.y + offset, z: attachment.z };
                let current_radius = leaf_radius + radius_offset - 1;
                place_leaves_row(
                    random, foliage_pos, current_radius - 2, foliage_height - 3, grid, tags, self, provider,
                    foliage_positions, placed_any, double_trunk,
                );
                place_leaves_row(
                    random, foliage_pos, current_radius - 1, foliage_height - 4, grid, tags, self, provider,
                    foliage_positions, placed_any, double_trunk,
                );
                for y in (0..=foliage_height - 5).rev() {
                    place_leaves_row(
                        random, foliage_pos, current_radius, y, grid, tags, self, provider, foliage_positions,
                        placed_any, double_trunk,
                    );
                }
                place_leaves_row_with_hanging_leaves_below(
                    random, foliage_pos, current_radius, -1, double_trunk, *hanging_leaves_chance,
                    *hanging_leaves_extension_chance, grid, tags, self, provider, foliage_positions, placed_any,
                );
                place_leaves_row_with_hanging_leaves_below(
                    random, foliage_pos, current_radius - 1, -2, double_trunk, *hanging_leaves_chance,
                    *hanging_leaves_extension_chance, grid, tags, self, provider, foliage_positions, placed_any,
                );
            }
            // `RandomSpreadFoliagePlacer.createFoliage` — the one placer in
            // this module with no row structure at all. `origin =
            // foliageAttachment.pos()` DIRECTLY, ignoring the `offset`
            // parameter entirely (unlike every row-based placer above, which
            // reads `offset` before its own first draw) — that is real
            // vanilla behaviour, not an omission: `RandomSpreadFoliagePlacer
            // .createFoliage` never references its own `offset` parameter.
            // Each of `leaf_placement_attempts` iterations draws SIX ints
            // (`nextInt(leafRadius)` twice for `dx`, twice for `dz`, and
            // `nextInt(foliageHeight)` twice for `dy`, in that exact
            // interleaved x/y/z order) and attempts exactly one leaf.
            FoliagePlacerCfg::RandomSpread { leaf_placement_attempts, .. } => {
                for _ in 0..*leaf_placement_attempts {
                    let dx = random.next_int_bounded(leaf_radius) - random.next_int_bounded(leaf_radius);
                    let dy = random.next_int_bounded(foliage_height) - random.next_int_bounded(foliage_height);
                    let dz = random.next_int_bounded(leaf_radius) - random.next_int_bounded(leaf_radius);
                    let pos = BlockPos { x: attachment.x + dx, y: attachment.y + dy, z: attachment.z + dz };
                    try_place_leaf(random, pos, grid, tags, provider, foliage_positions, placed_any);
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn place_leaves_row<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    current_radius: i32,
    y: i32,
    grid: &mut VegGrid,
    tags: &VegTags,
    placer: &FoliagePlacerCfg,
    provider: &BlockStateProvider,
    foliage_positions: &mut FastSet<(i32, i32, i32)>,
    placed_any: &mut bool,
    double_trunk: bool,
) {
    // `FoliagePlacer.placeLeavesRow`: for a double trunk the loop is widened
    // by one in the positive direction (`offset = doubleTrunk ? 1 : 0`) to
    // cover the 2×2 trunk footprint, and every cell is gated by the placer's
    // SIGNED skip logic (the non-signed `should_skip_location` is only ever
    // reached through it).
    let offset = if double_trunk { 1 } else { 0 };
    for dx in -current_radius..=current_radius + offset {
        for dz in -current_radius..=current_radius + offset {
            if !placer.should_skip_location_signed(random, dx, y, dz, current_radius, double_trunk) {
                let pos = BlockPos {
                    x: origin.x + dx,
                    y: origin.y + y,
                    z: origin.z + dz,
                };
                try_place_leaf(random, pos, grid, tags, provider, foliage_positions, placed_any);
            }
        }
    }
}

/// `FoliagePlacer.placeLeavesRowWithHangingLeavesBelow` — cherry's own
/// extension of [`place_leaves_row`]: after placing the row
/// itself, walk its four edges (`Direction.Plane.HORIZONTAL`, the usual
/// NORTH/EAST/SOUTH/WEST order) and, wherever the row directly above
/// (queried through `foliage_positions` — this module's stand-in for real
/// vanilla's `FoliageSetter.isSet`, which only ever answers "did THIS tree's
/// own foliage pass set a leaf here") carries a leaf, roll one extra hanging
/// leaf immediately below and, if that one lands, a second one below that.
/// `log_pos` (`origin.below()`, fixed once for the whole call) is the
/// `distManhattan` anchor both rolls bail out past a distance of 7 from —
/// see [`try_place_extension`].
#[allow(clippy::too_many_arguments)]
fn place_leaves_row_with_hanging_leaves_below<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    current_radius: i32,
    y: i32,
    double_trunk: bool,
    hanging_leaves_chance: f32,
    hanging_leaves_extension_chance: f32,
    grid: &mut VegGrid,
    tags: &VegTags,
    placer: &FoliagePlacerCfg,
    provider: &BlockStateProvider,
    foliage_positions: &mut FastSet<(i32, i32, i32)>,
    placed_any: &mut bool,
) {
    place_leaves_row(
        random, origin, current_radius, y, grid, tags, placer, provider, foliage_positions, placed_any,
        double_trunk,
    );
    let offset = if double_trunk { 1 } else { 0 };
    let log_pos = BlockPos { x: origin.x, y: origin.y - 1, z: origin.z };

    // `Direction.Plane.HORIZONTAL` (NORTH, EAST, SOUTH, WEST), each paired
    // with its own `getClockWise()` (NORTH->EAST, EAST->SOUTH, SOUTH->WEST,
    // WEST->NORTH) and that direction's `getAxisDirection() == POSITIVE`
    // (EAST/SOUTH are positive-x/positive-z; WEST/NORTH are not).
    const ALONG_EDGE: [(i32, i32); 4] = [(0, -1), (1, 0), (0, 1), (-1, 0)];
    const TO_EDGE: [(i32, i32); 4] = [(1, 0), (0, 1), (-1, 0), (0, -1)];
    const TO_EDGE_POSITIVE: [bool; 4] = [true, true, false, false];

    for i in 0..4 {
        let along_edge = ALONG_EDGE[i];
        let to_edge = TO_EDGE[i];
        let offset_to_edge = if TO_EDGE_POSITIVE[i] { current_radius + offset } else { current_radius };
        let mut px = origin.x + to_edge.0 * offset_to_edge + along_edge.0 * (-current_radius);
        let py = origin.y + y - 1;
        let mut pz = origin.z + to_edge.1 * offset_to_edge + along_edge.1 * (-current_radius);
        let mut offset_along_edge = -current_radius;

        while offset_along_edge < current_radius + offset {
            let leaves_above = foliage_positions.contains(&(px, py + 1, pz));
            if leaves_above {
                let pos = BlockPos { x: px, y: py, z: pz };
                if try_place_extension(random, hanging_leaves_chance, log_pos, pos, grid, tags, provider, foliage_positions, placed_any) {
                    let pos2 = BlockPos { x: px, y: py - 1, z: pz };
                    try_place_extension(random, hanging_leaves_extension_chance, log_pos, pos2, grid, tags, provider, foliage_positions, placed_any);
                }
            }
            offset_along_edge += 1;
            px += along_edge.0;
            pz += along_edge.1;
        }
    }
}

/// `FoliagePlacer.tryPlaceExtension` — one hanging-leaf roll, bounded to
/// within 7 Manhattan blocks of `log_pos`. Draws `next_float()`
/// unconditionally once the distance gate passes (matching Java's `random
/// .nextFloat() > chance ? false : tryPlaceLeaf(...)`, which evaluates the
/// comparison before short-circuiting to [`try_place_leaf`]).
#[allow(clippy::too_many_arguments)]
fn try_place_extension<R: RandomSource>(
    random: &mut R,
    chance: f32,
    log_pos: BlockPos,
    pos: BlockPos,
    grid: &mut VegGrid,
    tags: &VegTags,
    provider: &BlockStateProvider,
    foliage_positions: &mut FastSet<(i32, i32, i32)>,
    placed_any: &mut bool,
) -> bool {
    let manhattan =
        (pos.x - log_pos.x).abs() + (pos.y - log_pos.y).abs() + (pos.z - log_pos.z).abs();
    if manhattan >= 7 {
        return false;
    }
    if random.next_float() > chance {
        false
    } else {
        try_place_leaf(random, pos, grid, tags, provider, foliage_positions, placed_any)
    }
}

pub(super) fn try_place_leaf<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    grid: &mut VegGrid,
    tags: &VegTags,
    provider: &BlockStateProvider,
    foliage_positions: &mut FastSet<(i32, i32, i32)>,
    placed_any: &mut bool,
) -> bool {
    // `!isPersistent && validTreePos`: nothing this engine ever places
    // during worldgen carries `persistent=true` (only a player placing a
    // leaf block by hand can set it), so the persistence half of the check
    // is unconditionally true here — not modelled as a separate branch.
    let existing = grid.get_id(pos.x, pos.y, pos.z);
    if !valid_tree_pos(grid, tags, pos.x, pos.y, pos.z) {
        return false;
    }
    let Some(state) = provider.get_state_id(grid, tags, random, pos) else {
        return false;
    };
    // The `waterlogged` fix-up, by id: `Rewrite::Waterlogged` answers `None` for a
    // leaf state that carries no such property, which is exactly what the old
    // `state.find("waterlogged=")` miss meant — leave the state alone. Unit 8;
    // this is the third of the three sites `docs/worldgen-state-interning.md`
    // names, and it used to allocate on every single leaf placed.
    let is_water_source = tags.has(grid.interner(), Tag::Water, existing);
    let state = tags
        .rewrite(
            grid.interner(),
            state,
            Rewrite::Waterlogged(is_water_source),
        )
        .unwrap_or(state);
    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
    // `FoliageSetter.set`'s own `foliage.add(pos.immutable())` — this
    // module's stand-in for real vanilla's per-tree foliage-position set, so
    // [`place_leaves_row_with_hanging_leaves_below`]'s `isSet` queries can
    // answer from something other than this tree's earlier writes leaking
    // into a later tree's query (the set is cleared per [`super::place::place_tree`]
    // call — see that function's own `FOLIAGE_POS` scratch buffer).
    foliage_positions.insert((pos.x, pos.y, pos.z));
    *placed_any = true;
    true
}
