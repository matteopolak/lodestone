//! The per-feature placement bodies: `simple_block`, `block_column`, `tree` and the
//! beehive decorator — everything the driver dispatches into once a feature has been
//! resolved into a [`super::config`] value.
//!
//! Moved here verbatim from `feature/vegetation.rs` by U16 Phase B.

use std::cell::RefCell;

use crate::feature::BlockPos;
use crate::rng::RandomSource;

use super::config::{BlockColumnConfig, BlockStateProvider, Decorator, TreeConfig, VegTags};
use super::grid::VegGrid;
use super::grid::census::bump as census_bump;
use super::ids::{Tag, tag_at};
use super::tree::{
    Attachment, TrunkPlacerCfg, place_dark_oak_trunk, place_fancy_trunk, place_forking_trunk,
    place_giant_trunk, place_mega_jungle_trunk, update_leaf_distances,
};

pub(super) fn place_simple_block<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    provider: &BlockStateProvider,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let Some(state) = provider.get_state_id(grid, tags, random, pos) else {
        census_bump(|c| c.simple_block_no_state += 1);
        return;
    };
    // `VegetationBlock.canSurvive`: the block below must support vegetation
    // — see module doc on why this is applied uniformly.
    //
    // This is the single most-executed rejection in the whole engine —
    // `docs/worldgen-vegetation-census.md` counts 74,745 of them in one
    // 136-chunk sweep, every one of which used to be an interner read guard, a
    // `split('[')` and a `HashSet<String>` probe. Unit 8 made it a bit test.
    if !tag_at(grid, tags, Tag::SupportsVegetation, pos.x, pos.y - 1, pos.z) {
        census_bump(|c| c.simple_block_unsupported_ground += 1);
        return;
    }
    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
}

thread_local! {
    /// Reusable scratch for [`place_block_column`]'s per-layer sampled heights.
    ///
    /// `const`-initialised so touching it never allocates, and taken-then-returned
    /// rather than borrowed across the body so a future nested placement would get
    /// a correct (merely allocating) fresh buffer instead of a panic.
    static LAYER_HEIGHTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    /// Reusable scratch for one tree's trunk positions — the
    /// `TreeFeature.place` `trunks` set that seeds `update_leaf_distances`' BFS.
    static TRUNKS: RefCell<Vec<BlockPos>> = const { RefCell::new(Vec::new()) };
    /// Reusable scratch for one tree's foliage attachments.
    static ATTACHMENTS: RefCell<Vec<Attachment>> = const { RefCell::new(Vec::new()) };
}

/// `BlockColumnFeature.place`: samples every layer's height up front (so the
/// RNG draw order is fixed regardless of how far the column actually
/// reaches), then walks `direction` from `origin` checking `allowed_placement`
/// at each *next* position (`origin` itself is never checked — only used as
/// the first placement slot) for up to the sampled total height, truncating
/// via [`truncate_layers`] the moment a check fails, then places each layer's
/// blocks in declared order.
pub(super) fn place_block_column<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &BlockColumnConfig,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    // Reused scratch, not a fresh `Vec` — see [`LAYER_HEIGHTS`]. The draws below
    // happen in the same order on the same provider list, so the sampled values
    // are unchanged; only where they are stored moved.
    let mut layer_heights = LAYER_HEIGHTS.take();
    layer_heights.clear();
    for (h, _) in &cfg.layers {
        layer_heights.push(h.sample(random));
    }
    let total_height: i32 = layer_heights.iter().sum();
    if total_height == 0 {
        LAYER_HEIGHTS.set(layer_heights);
        return;
    }
    let (dx, dy, dz) = cfg.direction;
    let mut probe = BlockPos {
        x: origin.x + dx,
        y: origin.y + dy,
        z: origin.z + dz,
    };
    let mut new_height = total_height;
    for y in 0..total_height {
        if !cfg.allowed_placement.test(grid, tags, probe) {
            new_height = y;
            break;
        }
        probe = BlockPos {
            x: probe.x + dx,
            y: probe.y + dy,
            z: probe.z + dz,
        };
    }
    if new_height < total_height {
        truncate_layers(&mut layer_heights, total_height, new_height, cfg.prioritize_tip);
    }
    let mut place_pos = origin;
    for (i, (_, provider)) in cfg.layers.iter().enumerate() {
        for _ in 0..layer_heights[i] {
            if let Some(state) = provider.get_state_id(grid, tags, random, place_pos) {
                grid.set_id_if_in_bounds(place_pos.x, place_pos.y, place_pos.z, state);
            }
            place_pos = BlockPos {
                x: place_pos.x + dx,
                y: place_pos.y + dy,
                z: place_pos.z + dz,
            };
        }
    }
    LAYER_HEIGHTS.set(layer_heights);
}

/// `BlockColumnFeature.truncate`: removes `total_height - new_height` blocks
/// total, walking layers tip-first (`prioritize_tip`) or base-first
/// (everything else) — matching vanilla's own iteration-order choice exactly.
pub(super) fn truncate_layers(layer_heights: &mut [i32], total_height: i32, new_height: i32, prioritize_tip: bool) {
    let mut to_remove = total_height - new_height;
    let n = layer_heights.len();
    // Unit 8: the index order used to be materialised into a `Vec` per call.
    // `prioritize_tip` walks `0..n`, everything else walks it reversed — the same
    // two orders, computed rather than collected.
    for k in 0..n {
        let i = if prioritize_tip { k } else { n - 1 - k };
        if to_remove <= 0 {
            break;
        }
        let this_layer = layer_heights[i];
        let removed = this_layer.min(to_remove);
        to_remove -= removed;
        layer_heights[i] -= removed;
    }
}

pub(super) fn place_tree<R: RandomSource>(
    random: &mut R,
    origin: BlockPos,
    cfg: &TreeConfig,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let tree_height = cfg.trunk_placer.get_tree_height(random);
    let foliage_height = cfg.foliage_placer.foliage_height(random, tree_height);
    let trunk_len = tree_height - foliage_height;
    let leaf_radius = cfg.foliage_placer.foliage_radius(random, trunk_len);

    // `rootPlacer` is always absent for every species this module
    // implements, so `trunkOrigin == origin`: `minY == origin.y`,
    // `maxY == origin.y + treeHeight + 1`.
    if origin.y < grid.min_y + 1 || origin.y + tree_height + 1 > grid.min_y + grid.height + 1 {
        return;
    }

    // `getMaxFreeTreeHeight`: scan the tree's own footprint for anything
    // that isn't air/replaceable-by-trees/a log (a log counts as "free" —
    // `TrunkPlacer.isFree` — so an already-placed neighbour trunk doesn't
    // block this one). `ignore_vines` is `true` for every species here, so
    // the vine half of vanilla's check never applies.
    let mut clipped = tree_height;
    'scan: for y in 0..=tree_height + 1 {
        let r = cfg.feature_size.size_at_height(tree_height, y);
        for dx in -r..=r {
            for dz in -r..=r {
                // Unit 8: one grid read and up to three bit tests, where this
                // used to be an interner read guard plus three `HashSet<String>`
                // probes — and this scan runs over the tree's whole footprint on
                // every attempt, including the ones that reject.
                //
                // `y` here is the loop's tree-relative offset and `clipped` is
                // derived from it, so the absolute position must NOT shadow it.
                let id = grid.get_id(origin.x + dx, origin.y + y, origin.z + dz);
                let interner = grid.interner();
                let free = tags.has(interner, Tag::Air, id)
                    || tags.has(interner, Tag::ReplaceableByTrees, id)
                    || tags.has(interner, Tag::Logs, id);
                if !free {
                    clipped = y - 2;
                    break 'scan;
                }
            }
        }
    }
    // `TreeFeature.doPlace`'s own accept gate: `clippedTreeHeight >=
    // treeHeight` (no obstruction at all — every species this module shipped
    // before issue #428's fancy-oak increment) OR (a `min_clipped_height` is
    // configured AND the clip didn't cut below it) — `fancy_oak`'s own `4`
    // is the only shipped config that sets this, so this second arm is new
    // territory: every earlier species can still ONLY pass via the first.
    let min_clipped_height = cfg.feature_size.min_clipped_height();
    let accepted =
        clipped >= tree_height || min_clipped_height.is_some_and(|m| clipped >= m);
    if !accepted {
        return;
    }
    // `clippedTreeHeight` — real vanilla passes THIS, not the original
    // `treeHeight`, to `trunkPlacer.placeTrunk` (`foliageHeight`/`leafRadius`
    // above already used the original, pre-clip `treeHeight`, matching
    // vanilla's own evaluation order). For every species other than fancy
    // oak, `clipped == tree_height` is the only way `accepted` can be true,
    // so this substitution changes nothing for them; fancy oak is the first
    // real user of a genuinely shorter, clipped trunk.
    let clipped_tree_height = clipped;

    // Marks where this tree's own writes begin, so `update_leaf_distances`
    // can later derive its bbox from exactly this tree's own `trunks ∪
    // foliage ∪ decorations` — see that function's own doc comment on why
    // the bbox must be this narrow (real vanilla's `updateLeaves` is scoped
    // the same way, to one tree at a time, not the whole grid).
    let dirty_start = grid.dirty_len();

    // Dispatch trunk placement by placer kind — `Straight`'s own
    // `placeBelowTrunkBlock` + single-column loop stayed inline here (this
    // module's original #406 shape, unchanged); `Forking` (issue #428)
    // delegates to `place_forking_trunk`, which does its own
    // `placeBelowTrunkBlock` call internally, matching `ForkingTrunkPlacer
    // .placeTrunk`'s own real structure. Both branches produce the same
    // `(Vec<Attachment>, Vec<BlockPos>, placed_log)` shape — the third being
    // every position `trunkSetter` actually fired at (issue #428's
    // `update_leaf_distances` BFS seed, see that function's doc comment) —
    // so the foliage loop below is written once, not once per trunk kind.
    // Unit 8: both buffers are reused thread-local scratch rather than a fresh
    // `Vec` pair per tree, and the two delegating placers now fill them in place
    // instead of returning owned `Vec`s. Nothing about what is pushed, or in what
    // order, changed — `trunk_positions` is still every position `trunkSetter`
    // fired at, in the same sequence, which is what `update_leaf_distances`' BFS
    // seed depends on.
    let mut trunk_positions = TRUNKS.take();
    let mut attachments = ATTACHMENTS.take();
    trunk_positions.clear();
    attachments.clear();
    let placed_log = match &cfg.trunk_placer {
        TrunkPlacerCfg::Straight { .. } => {
            if let Some(below_provider) = &cfg.below_trunk_provider {
                let below_pos = BlockPos {
                    x: origin.x,
                    y: origin.y - 1,
                    z: origin.z,
                };
                if let Some(state) = below_provider.get_state_id(grid, tags, random, below_pos) {
                    grid.set_id_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
                    trunk_positions.push(below_pos);
                }
            }
            let mut placed_log = false;
            for y in 0..clipped_tree_height {
                let pos = BlockPos {
                    x: origin.x,
                    y: origin.y + y,
                    z: origin.z,
                };
                let id = grid.get_id(pos.x, pos.y, pos.z);
                let interner = grid.interner();
                if tags.has(interner, Tag::Air, id)
                    || tags.has(interner, Tag::ReplaceableByTrees, id)
                {
                    if let Some(state) = cfg.trunk_provider.get_state_id(grid, tags, random, pos) {
                        grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
                        placed_log = true;
                        trunk_positions.push(pos);
                    }
                }
            }
            attachments.push(Attachment {
                pos: BlockPos {
                    x: origin.x,
                    y: origin.y + clipped_tree_height,
                    z: origin.z,
                },
                radius_offset: 0,
                double_trunk: false,
            });
            placed_log
        }
        TrunkPlacerCfg::Forking { .. } => place_forking_trunk(
            random,
            origin,
            clipped_tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
            &mut attachments,
            &mut trunk_positions,
        ),
        TrunkPlacerCfg::DarkOak { .. } => place_dark_oak_trunk(
            random,
            origin,
            clipped_tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
            &mut attachments,
            &mut trunk_positions,
        ),
        TrunkPlacerCfg::Giant { .. } => place_giant_trunk(
            random,
            origin,
            clipped_tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
            &mut attachments,
            &mut trunk_positions,
        ),
        TrunkPlacerCfg::MegaJungle { .. } => place_mega_jungle_trunk(
            random,
            origin,
            clipped_tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
            &mut attachments,
            &mut trunk_positions,
        ),
        TrunkPlacerCfg::Fancy { .. } => place_fancy_trunk(
            random,
            origin,
            clipped_tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
            &mut attachments,
            &mut trunk_positions,
        ),
    };

    // `foliageAttachments.forEach(a -> foliagePlacer.createFoliage(...))` —
    // the public per-attachment overload draws `this.offset(random)` FRESH
    // for EACH attachment (not once overall), so the fresh
    // `sample_offset` call must live INSIDE this loop. For `Straight`
    // (always exactly one attachment) this is behaviourally identical to
    // the pre-#428 single call it replaces — no draw-count change for
    // oak/birch/spruce/pine.
    let mut placed_leaf = false;
    for attachment in &attachments {
        let offset = cfg.foliage_placer.sample_offset(random);
        cfg.foliage_placer.create_foliage(
            random,
            attachment.pos,
            foliage_height,
            leaf_radius,
            offset,
            attachment.radius_offset,
            attachment.double_trunk,
            grid,
            tags,
            &cfg.foliage_provider,
            &mut placed_leaf,
        );
    }

    if !placed_log && !placed_leaf {
        TRUNKS.set(trunk_positions);
        ATTACHMENTS.set(attachments);
        return;
    }

    for decorator in &cfg.decorators {
        match decorator {
            Decorator::Beehive { probability } => {
                place_beehive_decorator(random, *probability, origin, tree_height, grid, tags);
            }
            Decorator::TrunkVine => {
                place_trunk_vine_decorator(random, &trunk_positions, grid, tags);
            }
            Decorator::AttachedToLogs { probability, block_provider, directions } => {
                place_attached_to_logs_decorator(
                    random,
                    &trunk_positions,
                    *probability,
                    block_provider,
                    directions,
                    grid,
                    tags,
                );
            }
            Decorator::Unsupported => {}
        }
    }

    // `TreeFeature.place`'s own final step, AFTER decorators — issue #428's
    // fix for the `distance=7`-forever gap named in
    // `update_leaf_distances`'s own doc comment. Draws no RNG (a pure grid
    // post-process), so it is safe to run unconditionally here regardless
    // of which branch above produced `trunk_positions`. The bbox is exactly
    // `BoundingBox.encapsulatingPositions(trunks ∪ foliage ∪ decorations)`
    // (no `rootPositions` — no root placer implemented) — every absolute
    // position this ONE tree call wrote, from `dirty_start` (captured right
    // before trunk placement began) to now (right after decorators ran).
    // `dirty_cell_ids`, not `dirty_cells`: the latter resolves every position's
    // state to a `&'static str` through the interner's read guard, and this loop
    // discards the state entirely — it only wants the coordinates. Unit 8.
    let mut bbox: Option<(i32, i32, i32, i32, i32, i32)> = None;
    for (x, y, z, _) in grid.dirty_cell_ids().skip(dirty_start) {
        bbox = Some(match bbox {
            None => (x, y, z, x, y, z),
            Some((min_x, min_y, min_z, max_x, max_y, max_z)) => {
                (min_x.min(x), min_y.min(y), min_z.min(z), max_x.max(x), max_y.max(y), max_z.max(z))
            }
        });
    }
    // `bbox` is `None` only if every write this tree attempted landed
    // outside `grid`'s own footprint (single-chunk mode, a lean/branch that
    // walked entirely off-chunk) — matching `placed_log`/`placed_leaf`
    // above tracking ATTEMPTS, not landed writes. Real vanilla's own bbox
    // is always non-empty here (its world has no footprint to fall outside
    // of), so this is a narrowing specific to this engine's bounded grid,
    // not a case vanilla itself has — nothing to update in that case.
    if let Some(bbox) = bbox {
        update_leaf_distances(grid, tags, &trunk_positions, bbox);
    }
    TRUNKS.set(trunk_positions);
    ATTACHMENTS.set(attachments);
}

/// `net.minecraft.world.level.levelgen.feature.treedecorators.BeehiveDecorator`,
/// approximated — see module doc's "Approximations, named" section. The
/// **log**-row half (`logs.getFirst()`/`getLast()`, i.e. the lowest/highest
/// log Y) is exact for this engine's straight trunks: exactly one log per Y
/// level, so "lowest"/"highest" is unambiguous regardless of Java `HashSet`
/// iteration order. The **leaf**-row half (`leaves.getFirst()`) has no such
/// invariant in general (a canopy has many leaves per Y row) — approximated
/// here as the canopy's topmost row, matching vanilla's own `hiveY` formula
/// shape (`max(topLeafRow - 1, topLogRow + 1)`) without vanilla's specific
/// (and not portably reproducible) choice of *which* leaf anchors it.
pub(super) fn place_beehive_decorator<R: RandomSource>(
    random: &mut R,
    probability: f32,
    origin: BlockPos,
    tree_height: i32,
    grid: &mut VegGrid,
    // Unit 8: needed only for the two air tests, which are now bit tests.
    tags: &VegTags,
) {
    // logs is never empty here (a straight trunk always tries to place at
    // least one log at y=0..tree_height, per place_tree above).
    if random.next_float() >= probability {
        return;
    }
    let logs_bottom_y = origin.y;
    let logs_top_y = origin.y + tree_height - 1;
    // Approximate "top leaf row" as the topmost row the foliage placer's own
    // `offset` reaches (the highest possible leaf Y for this tree).
    let leaves_top_y = origin.y + tree_height; // attachment.y, foliage's own highest reachable row (offset >= 0 for every species here)
    let hive_y = (leaves_top_y - 1).max(logs_bottom_y + 1).min(logs_top_y);

    const SPAWN_DIRECTIONS: [(i32, i32); 3] = [(1, 0), (-1, 0), (0, -1)]; // east, west, north — all but south (the worldgen-fixed facing)
    // A fixed array, not a `Vec`: the list is exactly three long by construction
    // (it is `SPAWN_DIRECTIONS`), so `candidates.len()` below was already a
    // compile-time 3 and the heap allocation bought nothing. Unit 8.
    let mut candidates: [(i32, i32, i32); SPAWN_DIRECTIONS.len()] =
        SPAWN_DIRECTIONS.map(|(dx, dz)| (origin.x + dx, hive_y, origin.z + dz));

    // `Util.shuffle` on a fixed 3-element list — a Fisher-Yates pass draws
    // exactly 2 `nextInt` calls regardless of list contents, so the RNG-draw
    // *count* here is exact even though the resulting order need not match
    // vanilla's own (which starts from a differently-ordered candidate list
    // in the ambiguous-iteration-order case this module already named).
    for i in (1..candidates.len()).rev() {
        let j = random.next_int_bounded(i as i32 + 1) as usize;
        candidates.swap(i, j);
    }

    let Some(&(hx, hy, hz)) = candidates.iter().find(|&&(x, y, z)| {
        tag_at(grid, tags, Tag::Air, x, y, z) && tag_at(grid, tags, Tag::Air, x, y, z + 1)
    }) else {
        return;
    };

    // Interned rather than allocated — the name is a constant, so the `String` it
    // used to build was pure waste. Unit 8.
    let state = grid.interner().id_of("minecraft:bee_nest[facing=south,honey_level=0]");
    grid.set_id_if_in_bounds(hx, hy, hz, state);
    // Issue #520: the bees. This draw was already here and its result was
    // discarded — the nest reached the client empty — and the fix was never to add
    // a draw but to start using one.
    let bee_count = 2 + random.next_int_bounded(2);
    // **`nextInt(599)` per bee is a NEW draw**, and that is the one behavioural
    // risk in this change: `BeehiveDecorator.place` really does call
    // `Occupant.create(random.nextInt(599))` in a loop
    // (`BeehiveDecorator.java:59`), so omitting it left this engine's stream
    // 2-3 draws *short* of vanilla's after every hive. Adding them moves this
    // engine toward vanilla and moves every later feature in the same step; the
    // JVM parity fixtures are what arbitrate whether that landed correctly.
    let bees = (0..bee_count)
        .map(|_| crate::overworld::block_entities::BeeOccupant {
            ticks_in_hive: random.next_int_bounded(599),
            // `Occupant.create`'s constant. See that type's own doc for why it is
            // carried rather than implied.
            min_ticks_in_hive: 600,
        })
        .collect();
    grid.push_block_entity(crate::overworld::block_entities::GeneratedBlockEntity::Beehive {
        x: hx,
        y: hy,
        z: hz,
        bees,
    });
}

/// Both [`TreeDecorator`]-family functions below share one input shape with
/// real vanilla's `TreeDecorator.Context`: `context.logs()` is built from a
/// `Set<BlockPos>` (this module's own insertion-order approximation of the
/// same ambiguous-iteration-order ground the beehive decorator above already
/// names) and then SORTED BY Y ascending — a real, non-approximated step
/// (`Comparator.comparingInt(Vec3i::getY)`, a stable sort). For a fallen
/// tree's own horizontal log that sort is a no-op (every position shares one
/// Y), but a straight vertical trunk (`jungle_tree`/`mega_jungle_tree`'s own
/// `trunk_vine` decorator) has one Y per level, so the sort is load-bearing
/// there. Both functions below sort a **copy**, matching `Context`'s own
/// `new ObjectArrayList<>(set)` — the caller's `trunk_positions`/log buffer
/// is untouched.
///
/// [`TreeDecorator`]: net.minecraft.world.level.levelgen.feature.treedecorators.TreeDecorator
fn y_sorted(logs: &[BlockPos]) -> Vec<BlockPos> {
    let mut sorted = logs.to_vec();
    sorted.sort_by_key(|p| p.y);
    sorted
}

/// `TrunkVineDecorator.place` — a hanging vine on each of `logs`' four
/// horizontal neighbours (west, east, north, south, in that exact order),
/// each gated by its OWN independent `random.nextInt(3) > 0` coin flip.
/// Every draw happens regardless of outcome (Java evaluates `nextInt(3)`
/// before the `> 0` test, so the draw is never skipped), and regardless of
/// whether the neighbour turns out to be air.
pub(super) fn place_trunk_vine_decorator<R: RandomSource>(
    random: &mut R,
    logs: &[BlockPos],
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    // (neighbour offset, the vine property SET on that neighbour — it clings
    // back toward the log, so e.g. the log's WEST neighbour gets `east=true`).
    const SIDES: [((i32, i32), &str); 4] =
        [((-1, 0), "east"), ((1, 0), "west"), ((0, -1), "south"), ((0, 1), "north")];
    for pos in y_sorted(logs) {
        for ((dx, dz), prop) in SIDES {
            if random.next_int_bounded(3) > 0 {
                let (nx, nz) = (pos.x + dx, pos.z + dz);
                if tag_at(grid, tags, Tag::Air, nx, pos.y, nz) {
                    grid.set_if_in_bounds(nx, pos.y, nz, format!("minecraft:vine[{prop}=true]"));
                }
            }
        }
    }
}

/// `AttachedToLogsDecorator.place` — one block (a mushroom, for every
/// shipped `fallen_*_tree` config) on a random direction off a random log.
/// `Util.shuffledCopy` is a real Fisher-Yates pass over the Y-sorted log
/// list (`i` from `logs.len()` down to `2`, `logs.len() - 1` draws total —
/// zero for a one-log stump, matching real vanilla exactly), THEN, per log
/// in the shuffled order: one direction draw, one probability draw (always,
/// even when the direction/air check that follows would reject), and only
/// on success a state-provider draw (a `weighted_state_provider`'s own
/// single `nextInt`, for every shipped instance).
#[allow(clippy::too_many_arguments)]
pub(super) fn place_attached_to_logs_decorator<R: RandomSource>(
    random: &mut R,
    logs: &[BlockPos],
    probability: f32,
    block_provider: &BlockStateProvider,
    directions: &[(i32, i32, i32)],
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    if directions.is_empty() {
        return;
    }
    let mut shuffled = y_sorted(logs);
    let n = shuffled.len();
    for i in (2..=n).rev() {
        let j = random.next_int_bounded(i as i32) as usize;
        shuffled.swap(i - 1, j);
    }
    for pos in shuffled {
        let idx = random.next_int_bounded(directions.len() as i32) as usize;
        let (dx, dy, dz) = directions[idx];
        let target = BlockPos { x: pos.x + dx, y: pos.y + dy, z: pos.z + dz };
        if random.next_float() <= probability && tag_at(grid, tags, Tag::Air, target.x, target.y, target.z) {
            if let Some(state) = block_provider.get_state_id(grid, tags, random, target) {
                grid.set_id_if_in_bounds(target.x, target.y, target.z, state);
            }
        }
    }
}
