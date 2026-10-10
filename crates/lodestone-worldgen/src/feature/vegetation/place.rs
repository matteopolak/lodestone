//! The per-feature placement bodies: `simple_block`, `block_column`, `tree` and the
//! beehive decorator — everything the driver dispatches into once a feature has been
//! resolved into a [`super::config`] value.
//!
//! Moved here verbatim from `feature/vegetation.rs` by U16 Phase B.

use std::cell::RefCell;

use lodestone_worldgen_core::hash::FastSet;

use crate::feature::BlockPos;
use crate::rng::RandomSource;
use lodestone_data::block::Block;
use lodestone_data::block_properties::{BuiltinPropertyValue, Properties, PropertyKey};
use lodestone_data::block_states::StateId;

use super::config::{BlockColumnConfig, BlockStateProvider, Decorator, TreeConfig, VegTags};
use super::grid::VegGrid;
use super::ids::{Tag, tag_at};
use super::tree::{
    Attachment, TrunkPlacerCfg, place_cherry_trunk, place_dark_oak_trunk, place_fancy_trunk,
    place_forking_trunk, place_giant_trunk, place_mega_jungle_trunk, update_leaf_distances,
};

pub(super) fn place_simple_block<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    provider: &BlockStateProvider,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let Some(state) = provider.get_state_id(grid, tags, random, pos) else {
        return;
    };
    // Resolve the target state's own survival family: vegetation requires the
    // compact supports_vegetation approximation, while support-free feature
    // states such as potent sulfur do not.
    if !super::features::simple_block_can_survive(grid, tags, state, pos) {
        return;
    }
    if let Some(upper) = double_plant_upper_state(grid, state) {
        // A two-block plant is one placement. The upper cell must be empty
        // before either half is written; rejecting after the lower write would
        // leave an impossible orphan and would make later features observe a
        // block that the placement never produced.
        if grid.get_id(pos.x, pos.y + 1, pos.z) != StateId::AIR {
            return;
        }
        grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
        grid.set_id_if_in_bounds(pos.x, pos.y + 1, pos.z, upper);
        return;
    }
    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state);
}

/// Maps the lower state of each ordinary two-block plant to its upper state.
///
/// These are contiguous pairs in the canonical 26.2 state table, with upper
/// immediately before lower. Checking the exact canonical ids keeps this hot
/// placement path free of block-name and property-string comparisons.
fn double_plant_upper_state(_grid: &VegGrid, lower: StateId) -> Option<StateId> {
    const LOWER: [u32; 6] = [12916, 12918, 12920, 12922, 12924, 12926];
    let index = LOWER.iter().position(|&id| id == lower.raw())?;
    StateId::new(LOWER[index] - 1)
}

thread_local! {
    /// Reusable scratch for [`place_block_column`]'s per-layer sampled heights.
    ///
    /// `const`-initialised so touching it never allocates, and taken-then-returned
    /// rather than borrowed across the body so a future nested placement would get
    /// a correct (merely allocating) fresh buffer instead of a panic.
    static LAYER_HEIGHTS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    /// Reusable scratch for one tree's trunk positions — vanilla's own
    /// tree-feature place's `trunks` set that seeds `update_leaf_distances`' BFS.
    static TRUNKS: RefCell<Vec<BlockPos>> = const { RefCell::new(Vec::new()) };
    /// Reusable scratch for one tree's foliage attachments.
    static ATTACHMENTS: RefCell<Vec<Attachment>> = const { RefCell::new(Vec::new()) };
    /// Reusable scratch for one tree's placed foliage positions — this
    /// module's stand-in for real vanilla's own foliage-setter "is set" check
    /// (its own tree-feature place's `foliage` `Set<BlockPos>`), which
    /// [`FoliagePlacerCfg::Cherry`](super::tree::FoliagePlacerCfg::Cherry)'s
    /// hanging-leaves-below rows query. Cleared once per [`place_tree`] call
    /// (scoped to the WHOLE tree, matching vanilla — not per attachment).
    static FOLIAGE_POS: RefCell<FastSet<(i32, i32, i32)>> = RefCell::new(FastSet::default());
    /// Reusable scratch for [`place_roots`]'s per-direction root simulation.
    static ROOT_POSITIONS: RefCell<Vec<BlockPos>> = const { RefCell::new(Vec::new()) };
}

/// Vanilla's own block-column feature's place: samples every layer's height up front (so the
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

/// Vanilla's own block-column feature's truncate: removes `total_height - new_height` blocks
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
    ROOT_POSITIONS.with(|roots| roots.borrow_mut().clear());
    let tree_height = cfg.trunk_placer.get_tree_height(random);
    let foliage_height = cfg.foliage_placer.foliage_height(random, tree_height);
    let trunk_len = tree_height - foliage_height;
    let leaf_radius = cfg.foliage_placer.foliage_radius(random, trunk_len);

    let trunk_origin = origin;

    // Vanilla's own min/max Y span: the lower of the two origins' Y, and the
    // higher of the two plus the tree height plus one.
    let min_y = origin.y.min(trunk_origin.y);
    let max_y = origin.y.max(trunk_origin.y) + tree_height + 1;
    if min_y < grid.min_y + 1 || max_y > grid.min_y + grid.height + 1 {
        return;
    }

    // Vanilla's own max-free-tree-height: scan the tree's own footprint (anchored at
    // `trunk_origin`, not `origin` — real vanilla passes `trunkOrigin` here
    // too) for anything that isn't air/replaceable-by-trees/a log (a log
    // counts as "free" — vanilla's own trunk-placer "is free" check — so an already-placed
    // neighbour trunk doesn't block this one). `ignore_vines` is `true` for
    // every species here, so the vine half of vanilla's check never applies.
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
                let id = grid.get_id(trunk_origin.x + dx, trunk_origin.y + y, trunk_origin.z + dz);
                let free = tags.has(Tag::Air, id)
                    || tags.has(Tag::ReplaceableByTrees, id)
                    || tags.has(Tag::Logs, id);
                if !free {
                    clipped = y - 2;
                    break 'scan;
                }
            }
        }
    }
    // Vanilla's own tree-feature inner place step's own accept gate: `clippedTreeHeight >=
    // treeHeight` (no obstruction at all — every species this module shipped
    // before fancy-oak was added) OR (a `min_clipped_height` is
    // configured AND the clip didn't cut below it) — `fancy_oak`'s own `4`
    // is the only shipped config that sets this, so this second arm is new
    // territory: every earlier species can still ONLY pass via the first.
    let min_clipped_height = cfg.feature_size.min_clipped_height();
    let accepted =
        clipped >= tree_height || min_clipped_height.is_some_and(|m| clipped >= m);
    if !accepted {
        return;
    }
    // The clipped tree height — real vanilla passes THIS, not the original
    // tree height, to its own trunk-placer place-trunk (foliage height and leaf radius
    // above already used the original, pre-clip height, matching
    // vanilla's own evaluation order). For every species other than fancy
    // oak, `clipped == tree_height` is the only way `accepted` can be true,
    // so this substitution changes nothing for them; fancy oak is the first
    // real user of a genuinely shorter, clipped trunk.
    let clipped_tree_height = clipped;

    // Marks where this tree's own writes begin, so `update_leaf_distances`
    // can later derive its bbox from exactly this tree's own `roots ∪ trunks
    // ∪ foliage ∪ decorations` — see that function's own doc comment on why
    // the bbox must be this narrow (real vanilla's leaf-distance update is scoped
    // the same way, to one tree at a time, not the whole grid). Captured
    // BEFORE root placement, which is the first thing that can write.
    let dirty_start = grid.dirty_len();

    // Dispatch trunk placement by placer kind — `Straight`'s own
    // Vanilla's own place-below-trunk-block step plus a single-column loop
    // stayed inline here (this module's original shape, unchanged); `Forking`
    // delegates to `place_forking_trunk`, which does its own place-below-
    // trunk-block call internally, matching vanilla's own forking trunk
    // placer structure. Both branches produce the same
    // `(Vec<Attachment>, Vec<BlockPos>, placed_log)` shape — the third being
    // every position the trunk-setter callback actually fired at (the
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
                    x: trunk_origin.x,
                    y: trunk_origin.y - 1,
                    z: trunk_origin.z,
                };
                if let Some(state) = below_provider.get_state_id(grid, tags, random, below_pos) {
                    grid.set_id_if_in_bounds(below_pos.x, below_pos.y, below_pos.z, state);
                    trunk_positions.push(below_pos);
                }
            }
            let mut placed_log = false;
            for y in 0..clipped_tree_height {
                let pos = BlockPos {
                    x: trunk_origin.x,
                    y: trunk_origin.y + y,
                    z: trunk_origin.z,
                };
                let id = grid.get_id(pos.x, pos.y, pos.z);
                if tags.has(Tag::Air, id)
                    || tags.has(Tag::ReplaceableByTrees, id)
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
                    x: trunk_origin.x,
                    y: trunk_origin.y + clipped_tree_height,
                    z: trunk_origin.z,
                },
                radius_offset: 0,
                double_trunk: false,
            });
            placed_log
        }
        TrunkPlacerCfg::Forking { .. } => place_forking_trunk(
            random,
            trunk_origin,
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
            trunk_origin,
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
            trunk_origin,
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
            trunk_origin,
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
            trunk_origin,
            clipped_tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
            &mut attachments,
            &mut trunk_positions,
        ),
        TrunkPlacerCfg::Cherry {
            branch_count,
            branch_horizontal_length,
            branch_start_offset_from_top,
            branch_end_offset_from_top,
            ..
        } => place_cherry_trunk(
            random,
            trunk_origin,
            clipped_tree_height,
            grid,
            tags,
            &cfg.trunk_provider,
            &cfg.below_trunk_provider,
            branch_count,
            branch_horizontal_length,
            *branch_start_offset_from_top,
            branch_end_offset_from_top,
            &mut attachments,
            &mut trunk_positions,
        ),
    };

    // `foliageAttachments.forEach(a -> foliagePlacer.create_foliage(...))` —
    // the public per-attachment overload draws `this.offset(random)` FRESH
    // for EACH attachment (not once overall), so the fresh
    // `sample_offset` call must live INSIDE this loop. For `Straight`
    // (always exactly one attachment) this is behaviourally identical to
    // the single call it replaces from before the savanna/acacia increment
    // — no draw-count change for
    // oak/birch/spruce/pine.
    // `foliage_positions` is this module's stand-in for real vanilla's own
    // foliage-setter "is set" check — see [`FOLIAGE_POS`]'s own doc. Scoped to the
    // WHOLE tree (cleared here, read/written across every attachment's
    // `create_foliage` call), matching vanilla's own tree-feature place's single `foliage`
    // set shared by every `foliageAttachment`.
    let mut foliage_positions = FOLIAGE_POS.take();
    foliage_positions.clear();
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
            &mut foliage_positions,
            &mut placed_leaf,
        );
    }
    FOLIAGE_POS.set(foliage_positions);

    if !placed_log && !placed_leaf {
        TRUNKS.set(trunk_positions);
        ATTACHMENTS.set(attachments);
        return;
    }

    for decorator in &cfg.decorators {
        match decorator {
            Decorator::Beehive { probability } => {
                place_beehive_decorator(random, *probability, trunk_origin, tree_height, grid, tags);
            }
            Decorator::AlterGround { provider } => {
                ROOT_POSITIONS.with(|roots| {
                    place_alter_ground_decorator(
                        random,
                        &trunk_positions,
                        &roots.borrow(),
                        provider,
                        grid,
                        tags,
                    );
                });
            }
            Decorator::TrunkVine => {
                place_trunk_vine_decorator(random, &trunk_positions, grid, tags);
            }
            Decorator::Unsupported => {}
        }
    }

    // Vanilla's own tree-feature place's final step, AFTER decorators — the
    // fix for the `distance=7`-forever gap named in
    // `update_leaf_distances`'s own doc comment. Draws no RNG (a pure grid
    // post-process), so it is safe to run unconditionally here regardless
    // of which branch above produced `trunk_positions`. The bbox is exactly
    // vanilla's own encapsulating-positions bound over roots ∪ trunks ∪ foliage ∪
    // decorations — every absolute position this ONE tree call wrote, from
    // `dirty_start` (captured right before root placement began) to now
    // (right after decorators ran). Root positions are not part of
    // `update_leaf_distances`'s own BFS seed (only `trunk_positions` is —
    // matching vanilla, whose own BFS seed list is built from logs alone, never
    // root positions), but they are still part of the bbox this loop derives
    // from `grid`'s own dirty range, exactly as vanilla's own
    // encapsulating-positions bound includes them.
    // The state payload is intentionally ignored; this loop only needs the
    // coordinates written by this tree.
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
    TRUNKS.set(trunk_positions);
    ATTACHMENTS.set(attachments);
}

/// Alters eligible ground beneath every lowest trunk/root position.
fn place_alter_ground_decorator<R: RandomSource>(
    random: &mut R,
    logs: &[BlockPos],
    roots: &[BlockPos],
    provider: &BlockStateProvider,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    let Some(lowest_y) = logs.iter().chain(roots).map(|pos| pos.y).min() else {
        return;
    };
    for origin in logs.iter().chain(roots).filter(|pos| pos.y == lowest_y) {
        for (dx, dz) in [(-1, -1), (2, -1), (-1, 2), (2, 2)] {
            place_alter_ground_circle(
                random,
                BlockPos { x: origin.x + dx, y: origin.y, z: origin.z + dz },
                provider,
                grid,
                tags,
            );
        }
        for _ in 0..5 {
            let placement = random.next_int_bounded(64);
            let x = placement % 8;
            let z = placement / 8;
            if x == 0 || x == 7 || z == 0 || z == 7 {
                place_alter_ground_circle(
                    random,
                    BlockPos {
                        x: origin.x - 3 + x,
                        y: origin.y,
                        z: origin.z - 3 + z,
                    },
                    provider,
                    grid,
                    tags,
                );
            }
        }
    }
}

fn place_alter_ground_circle<R: RandomSource>(
    random: &mut R,
    center: BlockPos,
    provider: &BlockStateProvider,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    for dx in -2_i32..=2 {
        for dz in -2_i32..=2 {
            if dx.abs() == 2 && dz.abs() == 2 {
                continue;
            }
            place_alter_ground_at(
                random,
                BlockPos { x: center.x + dx, y: center.y, z: center.z + dz },
                provider,
                grid,
                tags,
            );
        }
    }
}

fn place_alter_ground_at<R: RandomSource>(
    random: &mut R,
    pos: BlockPos,
    provider: &BlockStateProvider,
    grid: &mut VegGrid,
    tags: &VegTags,
) {
    for dy in (-3..=2).rev() {
        let cursor = BlockPos { y: pos.y + dy, ..pos };
        if let Some(state) = provider.get_state_id(grid, tags, random, cursor) {
            grid.set_id_if_in_bounds(cursor.x, cursor.y, cursor.z, state);
            break;
        }
        if dy < 0 && !tag_at(grid, tags, Tag::Air, cursor.x, cursor.y, cursor.z) {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::XoroshiroPositionalFactory;

    fn state(spec: &str) -> StateId {
        StateId::from_state_str(spec).expect("test state is in the generated table")
    }

    struct ScriptedRandom {
        ints: Vec<i32>,
        cursor: usize,
    }

    impl ScriptedRandom {
        fn new(ints: &[i32]) -> Self {
            Self { ints: ints.to_vec(), cursor: 0 }
        }
    }

    impl RandomSource for ScriptedRandom {
        type Positional = XoroshiroPositionalFactory;

        fn fork_positional(&mut self) -> Self::Positional { panic!("fixture does not fork") }
        fn set_seed(&mut self, _seed: i64) { panic!("fixture does not reseed") }
        fn next_bits(&mut self, _bits: u32) -> i32 { panic!("fixture does not draw bits") }
        fn next_int(&mut self) -> i32 { panic!("fixture does not draw unbounded ints") }
        fn next_int_bounded(&mut self, bound: i32) -> i32 {
            let value = self.ints[self.cursor];
            self.cursor += 1;
            assert!((0..bound).contains(&value));
            value
        }
        fn next_long(&mut self) -> i64 { panic!("fixture does not draw longs") }
        fn next_bool(&mut self) -> bool { panic!("fixture does not draw bools") }
        fn next_float(&mut self) -> f32 { panic!("fixture does not draw floats") }
        fn next_double(&mut self) -> f64 { panic!("fixture does not draw doubles") }
        fn next_gaussian(&mut self) -> f64 { panic!("fixture does not draw gaussians") }
        fn consume_count(&mut self, _rounds: u32) { panic!("fixture does not consume rounds") }
    }

    fn podzol_fixture() -> (VegGrid<'static>, VegTags, BlockStateProvider) {
        let mut grid = VegGrid::new(0, 12, 0, 0);
        for x in 0..16 {
            for z in 0..16 {
                grid.seed_id(x, 4, z, state("minecraft:dirt"));
            }
        }
        let mut tags = VegTags::default();
        tags.beneath_tree_podzol_replaceable.insert(Block::Dirt);
        tags.bind();
        let provider = BlockStateProvider::RuleBased {
            rules: vec![(
                super::super::config::BlockPredicate::MatchingBlockTag {
                    tag: Some(super::super::ids::Tag::BeneathTreePodzolReplaceable),
                    offset: (0, 0, 0),
                },
                Box::new(BlockStateProvider::simple("minecraft:podzol[snowy=false]")),
            )],
            fallback: None,
        };
        (grid, tags, provider)
    }

    #[test]
    fn alter_ground_literal_shape_and_rng_fixture() {
        let (mut grid, tags, provider) = podzol_fixture();
        // No perimeter draw is admitted. The fixture therefore isolates the
        // four fixed rounded patches and proves the unconditional five draws.
        let mut random = ScriptedRandom::new(&[9, 18, 27, 36, 45]);
        place_alter_ground_decorator(
            &mut random,
            &[BlockPos { x: 7, y: 5, z: 7 }],
            &[],
            &provider,
            &mut grid,
            &tags,
        );
        assert_eq!(random.cursor, 5);
        // Union of the four rounded 5x5 patches, derived independently from
        // their literal centers and corner exclusion.
        let mut expected = std::collections::HashSet::new();
        for (cx, cz) in [(6, 6), (9, 6), (6, 9), (9, 9)] {
            for dx in -2_i32..=2 {
                for dz in -2_i32..=2 {
                    if dx.abs() != 2 || dz.abs() != 2 {
                        expected.insert((cx + dx, 4, cz + dz));
                    }
                }
            }
        }
        let actual: std::collections::HashSet<_> = grid
            .dirty_cells()
            .map(|(x, y, z, state_id)| {
                assert_eq!(state_id, state("minecraft:podzol[snowy=false]"));
                (x, y, z)
            })
            .collect();
        assert_eq!(actual, expected);
    }

}

/// Vanilla's own beehive tree-decorator,
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

    // Vanilla's own list-shuffle on a fixed 3-element list — a Fisher-Yates pass draws
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

    let properties = Properties::empty()
        .with_builtin(PropertyKey::Facing, BuiltinPropertyValue::South)
        .and_then(|properties| properties.with_builtin(PropertyKey::HoneyLevel, BuiltinPropertyValue::Value0))
        .expect("bee nest properties");
    let state = Properties::state_for_block(Block::BeeNest, &properties)
        .expect("bee nest state");
    grid.set_id_if_in_bounds(hx, hy, hz, state);
    // Two or three bees, each with a bounded `[0, 599)` hive-time draw. The
    // occupants are not carried anywhere (structure placement has no
    // block-entity channel, so the nest arrives empty), but the draws still
    // happen: every later feature element of the structure shares this stream.
    let bee_count = 2 + random.next_int_bounded(2);
    for _ in 0..bee_count {
        random.next_int_bounded(599);
    }
}

/// Both tree-decorator-family functions below share one input shape with
/// real vanilla's own tree-decorator context: `context.logs()` is built from a
/// `Set<BlockPos>` (this module's own insertion-order approximation of the
/// same ambiguous-iteration-order ground the beehive decorator above already
/// names) and then SORTED BY Y ascending — a real, non-approximated step
/// (vanilla's own ascending-Y comparator, a stable sort). For a fallen
/// tree's own horizontal log that sort is a no-op (every position shares one
/// Y), but a straight vertical trunk (`jungle_tree`/`mega_jungle_tree`'s own
/// `trunk_vine` decorator) has one Y per level, so the sort is load-bearing
/// there. Both functions below sort a **copy**, matching vanilla's own
/// context's own array-list-from-set construction — the caller's `trunk_positions`/log buffer
/// is untouched.
fn y_sorted(logs: &[BlockPos]) -> Vec<BlockPos> {
    let mut sorted = logs.to_vec();
    sorted.sort_by_key(|p| p.y);
    sorted
}

/// Vanilla's own trunk-vine decorator's place — a hanging vine on each of `logs`' four
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
    let vine = Block::Vine.default_state();
    for pos in y_sorted(logs) {
        for ((dx, dz), prop) in SIDES {
            if random.next_int_bounded(3) > 0 {
                let (nx, nz) = (pos.x + dx, pos.z + dz);
                if tag_at(grid, tags, Tag::Air, nx, pos.y, nz) {
                    let key = match prop {
                        "east" => PropertyKey::East,
                        "west" => PropertyKey::West,
                        "north" => PropertyKey::North,
                        "south" => PropertyKey::South,
                        _ => unreachable!(),
                    };
                    let properties = Properties::from_state_id(vine)
                        .with_builtin(key, BuiltinPropertyValue::True)
                        .expect("vine face property");
                    let state = Properties::state_for_block(Block::Vine, &properties)
                        .expect("vine face state");
                    grid.set_id_if_in_bounds(nx, pos.y, nz, state);
                }
            }
        }
    }
}

