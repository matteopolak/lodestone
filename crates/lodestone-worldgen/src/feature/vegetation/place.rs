//! The per-feature placement bodies: `simple_block`, `block_column`, `tree` and the
//! beehive decorator — everything the driver dispatches into once a feature has been
//! resolved into a [`super::config`] value.
//!
//! Moved here verbatim from `feature/vegetation.rs` by U16 Phase B.

use crate::feature::BlockPos;
use crate::rng::RandomSource;

use super::base_id;
use super::config::{
    BlockColumnConfig, BlockStateProvider, Decorator, TreeConfig, VegTags, is_air,
};
use super::grid::VegGrid;
use super::grid::census::bump as census_bump;
use super::tree::{
    Attachment, TrunkPlacerCfg, place_dark_oak_trunk, place_forking_trunk,
    update_leaf_distances,
};

pub(super) fn place_simple_block<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    provider: &BlockStateProvider,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let Some(state) = provider.get_state(grid, tags, random, pos) else {
        census_bump(|c| c.simple_block_no_state += 1);
        return;
    };
    // `VegetationBlock.canSurvive`: the block below must support vegetation
    // — see module doc on why this is applied uniformly.
    let below = base_id(grid.get(pos.x, pos.y - 1, pos.z));
    if !tags.supports_vegetation.contains(below) {
        census_bump(|c| c.simple_block_unsupported_ground += 1);
        return;
    }
    grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
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
    let mut layer_heights: Vec<i32> = cfg.layers.iter().map(|(h, _)| h.sample(random)).collect();
    let total_height: i32 = layer_heights.iter().sum();
    if total_height == 0 {
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
            if let Some(state) = provider.get_state(grid, tags, random, place_pos) {
                grid.set_if_in_bounds(place_pos.x, place_pos.y, place_pos.z, state);
            }
            place_pos = BlockPos {
                x: place_pos.x + dx,
                y: place_pos.y + dy,
                z: place_pos.z + dz,
            };
        }
    }
}

/// `BlockColumnFeature.truncate`: removes `total_height - new_height` blocks
/// total, walking layers tip-first (`prioritize_tip`) or base-first
/// (everything else) — matching vanilla's own iteration-order choice exactly.
pub(super) fn truncate_layers(layer_heights: &mut [i32], total_height: i32, new_height: i32, prioritize_tip: bool) {
    let mut to_remove = total_height - new_height;
    let n = layer_heights.len();
    let indices: Vec<usize> = if prioritize_tip {
        (0..n).collect()
    } else {
        (0..n).rev().collect()
    };
    for i in indices {
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
                let base = base_id(grid.get(origin.x + dx, origin.y + y, origin.z + dz));
                let free = is_air(base)
                    || tags.replaceable_by_trees.contains(base)
                    || tags.logs.contains(base);
                if !free {
                    clipped = y - 2;
                    break 'scan;
                }
            }
        }
    }
    if clipped < tree_height {
        return;
    }

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
    let (attachments, trunk_positions, placed_log) = match &cfg.trunk_placer {
        TrunkPlacerCfg::Straight { .. } => {
            let mut trunk_positions = Vec::new();
            if let Some(below_provider) = &cfg.below_trunk_provider {
                let below_pos = BlockPos {
                    x: origin.x,
                    y: origin.y - 1,
                    z: origin.z,
                };
                if let Some(state) = below_provider.get_state(grid, tags, random, below_pos) {
                    grid.set_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
                    trunk_positions.push(below_pos);
                }
            }
            let mut placed_log = false;
            for y in 0..tree_height {
                let pos = BlockPos {
                    x: origin.x,
                    y: origin.y + y,
                    z: origin.z,
                };
                let base = base_id(grid.get(pos.x, pos.y, pos.z));
                if is_air(base) || tags.replaceable_by_trees.contains(base) {
                    if let Some(state) = cfg.trunk_provider.get_state(grid, tags, random, pos) {
                        grid.set_if_in_bounds(pos.x, pos.y, pos.z, state);
                        placed_log = true;
                        trunk_positions.push(pos);
                    }
                }
            }
            let attachment = Attachment {
                pos: BlockPos {
                    x: origin.x,
                    y: origin.y + tree_height,
                    z: origin.z,
                },
                radius_offset: 0,
                double_trunk: false,
            };
            (vec![attachment], trunk_positions, placed_log)
        }
        TrunkPlacerCfg::Forking { .. } => place_forking_trunk(
            random,
            origin,
            tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
        ),
        TrunkPlacerCfg::DarkOak { .. } => place_dark_oak_trunk(
            random,
            origin,
            tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
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
        return;
    }

    for decorator in &cfg.decorators {
        match decorator {
            Decorator::Beehive { probability } => {
                place_beehive_decorator(random, *probability, origin, tree_height, grid);
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
    let mut bbox: Option<(i32, i32, i32, i32, i32, i32)> = None;
    for (x, y, z, _) in grid.dirty_cells().skip(dirty_start) {
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
    let mut candidates: Vec<(i32, i32, i32)> = SPAWN_DIRECTIONS
        .iter()
        .map(|(dx, dz)| (origin.x + dx, hive_y, origin.z + dz))
        .collect();

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
        is_air(base_id(grid.get(x, y, z))) && is_air(base_id(grid.get(x, y, z + 1)))
    }) else {
        return;
    };

    let state = "minecraft:bee_nest[facing=south,honey_level=0]".to_string();
    grid.set_if_in_bounds(hx, hy, hz, state);
    // Bee-entity storage (2-3 bees) is not modelled — this engine has no
    // block-entity/NBT layer for a freshly generated chunk to carry it in;
    // named here rather than silently pretending the hive is fully stocked.
    let _bee_count = 2 + random.next_int_bounded(2);
}
