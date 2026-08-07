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

/// `net.minecraft.world.level.levelgen.feature.trunkplacers.TrunkPlacer` (the
/// `Straight`/`Forking` subset — issue #428 adds `Forking`, acacia's real
/// trunk placer, alongside the `Straight` this module shipped with under
/// #406). Both variants carry the identical `(base_height, height_rand_a,
/// height_rand_b)` triple `TrunkPlacer.getTreeHeight` (a base-class method,
/// not overridden by either subclass) draws from — kept as one shared shape
/// rather than duplicating the three fields per variant.
#[derive(Clone, Copy, Debug)]
pub enum TrunkPlacerCfg {
    Straight {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
    /// `ForkingTrunkPlacer` — acacia's real trunk (issue #428): a single
    /// leaning column, plus (usually) one branch in a different horizontal
    /// direction. See [`place_trunk`] for the port of `placeTrunk` itself.
    Forking {
        base_height: i32,
        height_rand_a: i32,
        height_rand_b: i32,
    },
    /// `DarkOakTrunkPlacer` — dark oak's real trunk (issue #428): a 2×2 log
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
            _ => None,
        }
    }

    fn heights(&self) -> (i32, i32, i32) {
        match *self {
            Self::Straight { base_height, height_rand_a, height_rand_b }
            | Self::Forking { base_height, height_rand_a, height_rand_b }
            | Self::DarkOak { base_height, height_rand_a, height_rand_b } => {
                (base_height, height_rand_a, height_rand_b)
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

/// `ForkingTrunkPlacer.placeTrunk` — acacia's real trunk (issue #428).
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

/// `DarkOakTrunkPlacer.placeTrunk` — dark oak's real trunk (issue #428),
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
/// real, measured mismatch found by issue #428's savanna oracle fixtures
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
/// per-branch gaps" for why `Pine` is here despite not being one of issue
/// #406's three named species; `Acacia` is issue #428's addition, paired
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
    /// `AcaciaFoliagePlacer` — acacia's real foliage (issue #428). Its
    /// `foliageHeight` override always returns the constant `0`, drawing no
    /// RNG at all (unlike `Blob`'s config-constant `height` field or
    /// `Pine`'s sampled one) — see [`Self::foliage_height`]'s own arm.
    Acacia {
        radius: IntProvider,
        offset: IntProvider,
    },
    /// `DarkOakFoliagePlacer` — dark oak's real foliage (issue #428), paired
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
        }
    }

pub(super)     fn foliage_radius<R: RandomSource>(&self, random: &mut R, trunk_len: i32) -> i32 {
        match self {
            FoliagePlacerCfg::Blob { radius, .. }
            | FoliagePlacerCfg::Spruce { radius, .. }
            | FoliagePlacerCfg::Acacia { radius, .. }
            | FoliagePlacerCfg::DarkOak { radius, .. } => radius.sample(random),
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
            | FoliagePlacerCfg::DarkOak { offset, .. } => offset.sample(random),
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
        placed_any: &mut bool,
    ) {
        match self {
            FoliagePlacerCfg::Blob { .. } => {
                // `yo / 2` in both Java and Rust truncates toward zero, so
                // no special-casing is needed for negative `yo` here.
                for yo in (offset - foliage_height..=offset).rev() {
                    let radius = (leaf_radius - 1 - yo / 2).max(0);
                    place_leaves_row(
                        random, attachment, radius, yo, grid, tags, self, provider, placed_any, false,
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
                    random, attachment, leaf_radius + radius_offset, -1 - foliage_height, grid, tags, self, provider, placed_any, false,
                );
                place_leaves_row(random, attachment, leaf_radius - 1, -foliage_height, grid, tags, self, provider, placed_any, false);
                place_leaves_row(
                    random, attachment, leaf_radius + radius_offset - 1, 0, grid, tags, self, provider, placed_any, false,
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
                    place_leaves_row(random, pos, leaf_radius + 2, -1, grid, tags, self, provider, placed_any, true);
                    place_leaves_row(random, pos, leaf_radius + 3, 0, grid, tags, self, provider, placed_any, true);
                    place_leaves_row(random, pos, leaf_radius + 2, 1, grid, tags, self, provider, placed_any, true);
                    if random.next_bool() {
                        place_leaves_row(random, pos, leaf_radius, 2, grid, tags, self, provider, placed_any, true);
                    }
                } else {
                    place_leaves_row(random, pos, leaf_radius + 2, -1, grid, tags, self, provider, placed_any, false);
                    place_leaves_row(random, pos, leaf_radius + 1, 0, grid, tags, self, provider, placed_any, false);
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
                try_place_leaf(random, pos, grid, tags, provider, placed_any);
            }
        }
    }
}

pub(super) fn try_place_leaf<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    grid: &mut VegGrid,
    tags: &VegTags,
    provider: &BlockStateProvider,
    placed_any: &mut bool,
) {
    // `!isPersistent && validTreePos`: nothing this engine ever places
    // during worldgen carries `persistent=true` (only a player placing a
    // leaf block by hand can set it), so the persistence half of the check
    // is unconditionally true here — not modelled as a separate branch.
    let existing = grid.get_id(pos.x, pos.y, pos.z);
    if !valid_tree_pos(grid, tags, pos.x, pos.y, pos.z) {
        return;
    }
    let Some(state) = provider.get_state_id(grid, tags, random, pos) else {
        return;
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
    *placed_any = true;
}
