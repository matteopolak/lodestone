//! Pistons: the structure resolver, the quasi-connectivity signal rule, and the
//! move (issue #316).
//!
//! ## What it is
//!
//! Vanilla `PistonBaseBlock` + `PistonStructureResolver`, ported as pure decisions
//! over a `Fn(BlockPos) -> String` world lookup — the same shape every other
//! module in the `redstone*` family takes, so the wiring in
//! [`crate::random_tick`] reaches it exactly as it reaches a repeater.
//!
//! Nothing modelled pistons anywhere in this tree before: `lodestone-physics`'s
//! own comment noted it deliberately excludes `PISTON` as a type "this crate has
//! no equivalent of".
//!
//! ## How it works
//!
//! Three pieces, in dependency order.
//!
//! **1. [`push_reaction`] and [`is_pushable`].** `PushReaction` is per block, from
//! `Blocks.java`'s own `pushReaction(...)` calls — 200 `DESTROY`, 11 `BLOCK`, 16
//! `PUSH_ONLY` (the glazed terracottas), everything else `NORMAL` by default. The
//! four hard-coded exceptions in `PistonBaseBlock.isPushable` (obsidian, crying
//! obsidian, respawn anchor, reinforced deepslate) are exceptions *there*, not
//! `BLOCK` entries, and are reproduced as such.
//!
//! **2. [`resolve`] — `PistonStructureResolver`.** The 12-block limit, the
//! slime/honey sticky run, the perpendicular branching, and
//! `reorderListAtCollision`. **This is the order-sensitive part**, and the reason
//! the port is literal: the *order* of `to_push` decides which block ends up where
//! when two sticky lines collide, and an "obviously equivalent" rewrite of that
//! reorder produces a contraption that works for one shape and not another.
//!
//! **3. [`has_extend_signal`] — quasi-connectivity.** `getNeighborSignal` checks
//! the piston's own position from all six directions *except the push direction*,
//! then its own position from `DOWN`, then **`pos.above()`** from all directions
//! except `DOWN`. That last block is QC: a piston reacts to a signal one block
//! above itself that touches nothing else. Every BUD switch in the game is built
//! on it, and dropping it as "surely a bug" is how a port loses half the
//! contraptions.
//!
//! **4. The two-phase move.** [`begin_move`] and [`finish_move`] split
//! [`apply_move`]'s one-step writes into vanilla's `moving_piston` phase and its
//! commit, [`PISTON_MOVE_DELAY`] ticks later. The second phase is *derived from*
//! the one-step writes rather than recomputed, so the world two ticks after a push
//! is byte-identical to what the one-step path produced — the property
//! `two_phase_world_matches_the_one_step_path` asserts cell by cell.
//!
//! ## What is deliberately not here, and why #316 stays open
//!
//! The intermediate state now exists, so the two consequences that depended on
//! its absence are gone: a client is sent a real `moving_piston` cell plus its
//! block entity and animates the travel. What is still missing:
//!
//! * **A move is not interruptible.** Vanilla's `triggerEvent` calls `finalTick()`
//!   on a block entity it finds mid-animation and can start a second move in the
//!   same tick; here a pending commit runs to completion. A **0-tick pulse**
//!   depends on exactly that interruption, so it still cannot work.
//! * `TRIGGER_DROP` (block event 2) has no distinct behaviour: nothing routes a
//!   piston block *event* at all, so the "head is mid-extension, drop it" case is
//!   unreachable rather than merely unimplemented.
//! * Entities in the push path are not shoved: that is `PistonMovingBlockEntity`'s
//!   `moveCollidedEntities`/`moveStuckEntities`, which needs an entity AABB sweep
//!   this crate has no piston-aware collision pass for. The intermediate state it
//!   wanted is no longer the blocker; the collision pass is.
//! * A `moving_piston` cell has **no collision shape** here. Vanilla's
//!   `MovingPistonBlock.getCollisionShape` delegates to the block entity's
//!   interpolated shape, so a player rides a moving block; here the cell is empty
//!   for two ticks and a player standing on a pushed block briefly falls through.
//!
//! **A push cannot cross a chunk border.** [`crate::random_tick`]'s reaction
//! surface is column-local (`redstone::make_lookup` reads air outside its own
//! 16×16 footprint), so a run pushed across `x % 16 == 15` resolves against air.
//! That is a property of the whole redstone family here, not of this module, and
//! the resolver itself is border-agnostic — it is the lookup that is not.
//!
//! So: contraption **resolution** is faithful and tested; contraption **timing**
//! is not. Issue #316 asks for BUD-switch and 0-tick traces matched tick-for-tick
//! against a real 26.2 server, and that verification is unreachable while the
//! intermediate state does not exist.
//!
//! ## How to change it
//!
//! * **Do not "simplify" [`reorder_at_collision`].** It is three sublist splices
//!   in a specific order and the order is the behaviour.
//! * **Do not drop the `pos.above()` loop from [`has_extend_signal`].** See above.
//! * **Do not compute [`finish_move`]'s writes from [`resolve`] again.** They are a
//!   projection of [`apply_move`]'s output on purpose; recomputing them is how the
//!   animated path and the one-step path drift apart, and the drift would be
//!   invisible for every shape whose resolution happens to be stable.
//! * The commit is carried in the scheduled tick's *kind* string
//!   ([`finish_kind`]/[`parse_finish_kind`]), because the reaction surface a move
//!   runs on holds no block-entity map. Changing the encoding means changing both
//!   halves and the round-trip test between them.
//!
//! ## Dependencies
//!
//! [`crate::redstone`] for the signal query and the state-string helpers, and
//! [`crate::neighbor_update::Direction`]. No block-state census: push reaction is
//! per *block*, not per state, so a name table is the right shape.

use lodestone_model::BlockPos;

use crate::neighbor_update::ALL_DIRECTIONS;
// Re-exported: `MovingBlockEntity` and `piston_facing` both name this type in
// their public signatures, and `neighbor_update` itself is crate-private, so
// without this an outside caller (the v770 server protocol, which has to encode
// a moving piston's record) can hold one and never name its direction.
pub use crate::neighbor_update::Direction;
use crate::redstone;

/// `minecraft:piston`.
pub const PISTON: &str = "minecraft:piston";
/// `minecraft:sticky_piston`.
pub const STICKY_PISTON: &str = "minecraft:sticky_piston";
/// `minecraft:piston_head`.
pub const PISTON_HEAD: &str = "minecraft:piston_head";
/// `minecraft:slime_block` — one of the two sticky pull blocks.
pub const SLIME_BLOCK: &str = "minecraft:slime_block";
/// `minecraft:honey_block` — the other, which does **not** stick to slime.
pub const HONEY_BLOCK: &str = "minecraft:honey_block";

/// `PistonStructureResolver.MAX_PUSH_DEPTH`.
pub const MAX_PUSH_DEPTH: usize = 12;

/// The scheduled-tick kind a piston's extend/retract check runs under, in the same
/// namespace as [`redstone::TICK_REPEATER`] and friends.
pub const TICK_PISTON: &str = "redstone:piston";

/// Vanilla `PushReaction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushReaction {
    /// Moves.
    Normal,
    /// Breaks and drops.
    Destroy,
    /// Stops the piston.
    Block,
    /// Moves only when pushed along the piston's own axis (glazed terracotta).
    PushOnly,
}

/// Blocks whose `Properties` set `pushReaction(PushReaction.DESTROY)`.
static DESTROY: &[&str] = &[
    "minecraft:acacia_door", "minecraft:acacia_pressure_plate", "minecraft:acacia_sapling", "minecraft:allium",
    "minecraft:amethyst_cluster", "minecraft:attached_melon_stem", "minecraft:attached_pumpkin_stem", "minecraft:azalea",
    "minecraft:azure_bluet", "minecraft:bamboo", "minecraft:bamboo_door", "minecraft:bamboo_pressure_plate",
    "minecraft:bamboo_sapling", "minecraft:beetroots", "minecraft:bell", "minecraft:big_dripleaf",
    "minecraft:big_dripleaf_stem", "minecraft:birch_door", "minecraft:birch_pressure_plate", "minecraft:birch_sapling",
    "minecraft:blue_orchid", "minecraft:brain_coral", "minecraft:brain_coral_fan", "minecraft:brain_coral_wall_fan",
    "minecraft:brown_mushroom", "minecraft:bubble_column", "minecraft:bubble_coral", "minecraft:bubble_coral_fan",
    "minecraft:bubble_coral_wall_fan", "minecraft:budding_amethyst", "minecraft:bush", "minecraft:cactus",
    "minecraft:cactus_flower", "minecraft:cake", "minecraft:carrots", "minecraft:carved_pumpkin",
    "minecraft:cave_vines", "minecraft:cave_vines_plant", "minecraft:cherry_door", "minecraft:cherry_leaves",
    "minecraft:cherry_pressure_plate", "minecraft:cherry_sapling", "minecraft:chorus_flower", "minecraft:chorus_plant",
    "minecraft:closed_eyeblossom", "minecraft:cobweb", "minecraft:cocoa", "minecraft:comparator",
    "minecraft:copper_torch", "minecraft:copper_wall_torch", "minecraft:cornflower", "minecraft:creeper_head",
    "minecraft:creeper_wall_head", "minecraft:crimson_door", "minecraft:crimson_fungus", "minecraft:crimson_pressure_plate",
    "minecraft:crimson_roots", "minecraft:dandelion", "minecraft:dark_oak_door", "minecraft:dark_oak_pressure_plate",
    "minecraft:dark_oak_sapling", "minecraft:dead_bush", "minecraft:decorated_pot", "minecraft:dragon_egg",
    "minecraft:dragon_head", "minecraft:dragon_wall_head", "minecraft:fern", "minecraft:fire",
    "minecraft:fire_coral", "minecraft:fire_coral_fan", "minecraft:fire_coral_wall_fan", "minecraft:firefly_bush",
    "minecraft:flowering_azalea", "minecraft:frogspawn", "minecraft:glow_lichen", "minecraft:golden_dandelion",
    "minecraft:hanging_roots", "minecraft:heavy_weighted_pressure_plate", "minecraft:horn_coral", "minecraft:horn_coral_fan",
    "minecraft:horn_coral_wall_fan", "minecraft:iron_door", "minecraft:jack_o_lantern", "minecraft:jungle_door",
    "minecraft:jungle_pressure_plate", "minecraft:jungle_sapling", "minecraft:kelp", "minecraft:kelp_plant",
    "minecraft:ladder", "minecraft:lantern", "minecraft:large_fern", "minecraft:lava",
    "minecraft:leaf_litter", "minecraft:lever", "minecraft:light_weighted_pressure_plate", "minecraft:lilac",
    "minecraft:lily_of_the_valley", "minecraft:lily_pad", "minecraft:mangrove_door", "minecraft:mangrove_pressure_plate",
    "minecraft:mangrove_propagule", "minecraft:melon", "minecraft:melon_stem", "minecraft:moss_block",
    "minecraft:moss_carpet", "minecraft:nether_sprouts", "minecraft:nether_wart", "minecraft:oak_door",
    "minecraft:oak_pressure_plate", "minecraft:oak_sapling", "minecraft:open_eyeblossom", "minecraft:orange_tulip",
    "minecraft:oxeye_daisy", "minecraft:pale_hanging_moss", "minecraft:pale_moss_block", "minecraft:pale_moss_carpet",
    "minecraft:pale_oak_door", "minecraft:pale_oak_leaves", "minecraft:pale_oak_pressure_plate", "minecraft:pale_oak_sapling",
    "minecraft:peony", "minecraft:piglin_head", "minecraft:piglin_wall_head", "minecraft:pink_petals",
    "minecraft:pink_tulip", "minecraft:pitcher_crop", "minecraft:pitcher_plant", "minecraft:player_head",
    "minecraft:player_wall_head", "minecraft:pointed_dripstone", "minecraft:polished_blackstone_pressure_plate", "minecraft:poppy",
    "minecraft:potatoes", "minecraft:pumpkin", "minecraft:pumpkin_stem", "minecraft:red_mushroom",
    "minecraft:red_tulip", "minecraft:redstone_torch", "minecraft:redstone_wall_torch", "minecraft:redstone_wire",
    "minecraft:repeater", "minecraft:resin_clump", "minecraft:rose_bush", "minecraft:scaffolding",
    "minecraft:sculk_vein", "minecraft:sea_pickle", "minecraft:seagrass", "minecraft:short_dry_grass",
    "minecraft:short_grass", "minecraft:skeleton_skull", "minecraft:skeleton_wall_skull", "minecraft:small_dripleaf",
    "minecraft:snow", "minecraft:soul_fire", "minecraft:soul_lantern", "minecraft:soul_torch",
    "minecraft:soul_wall_torch", "minecraft:spore_blossom", "minecraft:spruce_door", "minecraft:spruce_pressure_plate",
    "minecraft:spruce_sapling", "minecraft:stone_pressure_plate", "minecraft:structure_void", "minecraft:sugar_cane",
    "minecraft:sulfur_spike", "minecraft:sunflower", "minecraft:suspicious_gravel", "minecraft:suspicious_sand",
    "minecraft:sweet_berry_bush", "minecraft:tall_dry_grass", "minecraft:tall_grass", "minecraft:tall_seagrass",
    "minecraft:torch", "minecraft:torchflower", "minecraft:torchflower_crop", "minecraft:tripwire",
    "minecraft:tripwire_hook", "minecraft:tube_coral", "minecraft:tube_coral_fan", "minecraft:tube_coral_wall_fan",
    "minecraft:turtle_egg", "minecraft:twisting_vines", "minecraft:twisting_vines_plant", "minecraft:vine",
    "minecraft:wall_torch", "minecraft:warped_door", "minecraft:warped_fungus", "minecraft:warped_pressure_plate",
    "minecraft:warped_roots", "minecraft:water", "minecraft:weeping_vines", "minecraft:weeping_vines_plant",
    "minecraft:wheat", "minecraft:white_tulip", "minecraft:wildflowers", "minecraft:wither_rose",
    "minecraft:wither_skeleton_skull", "minecraft:wither_skeleton_wall_skull", "minecraft:zombie_head", "minecraft:zombie_wall_head",
];

/// Blocks whose `Properties` set `pushReaction(PushReaction.BLOCK)`.
static BLOCKED: &[&str] = &[
    "minecraft:anvil", "minecraft:barrier", "minecraft:chipped_anvil", "minecraft:damaged_anvil",
    "minecraft:end_gateway", "minecraft:end_portal", "minecraft:grindstone", "minecraft:lodestone",
    "minecraft:moving_piston", "minecraft:nether_portal", "minecraft:piston_head",
];

/// `pushReaction(PushReaction.PUSH_ONLY)` — the sixteen glazed terracottas, the
/// only blocks in the game that use it.
static PUSH_ONLY: &[&str] = &[
    "minecraft:black_glazed_terracotta", "minecraft:blue_glazed_terracotta", "minecraft:brown_glazed_terracotta", "minecraft:cyan_glazed_terracotta",
    "minecraft:gray_glazed_terracotta", "minecraft:green_glazed_terracotta", "minecraft:light_blue_glazed_terracotta", "minecraft:light_gray_glazed_terracotta",
    "minecraft:lime_glazed_terracotta", "minecraft:magenta_glazed_terracotta", "minecraft:orange_glazed_terracotta", "minecraft:pink_glazed_terracotta",
    "minecraft:purple_glazed_terracotta", "minecraft:red_glazed_terracotta", "minecraft:white_glazed_terracotta", "minecraft:yellow_glazed_terracotta",
];

/// The `PushReaction` for a block state, defaulting to
/// [`Normal`](PushReaction::Normal) as `BlockBehaviour.Properties` does.
#[must_use]
pub fn push_reaction(state: &str) -> PushReaction {
    let base = redstone::base_name(state);
    if DESTROY.binary_search(&base).is_ok() {
        PushReaction::Destroy
    } else if BLOCKED.binary_search(&base).is_ok() {
        PushReaction::Block
    } else if PUSH_ONLY.contains(&base) {
        PushReaction::PushOnly
    } else {
        PushReaction::Normal
    }
}

// --- state helpers ---------------------------------------------------------

/// Whether `state` is either piston base.
#[must_use]
pub fn is_piston(state: &str) -> bool {
    matches!(redstone::base_name(state), PISTON | STICKY_PISTON)
}

/// Whether `state` is the sticky base.
#[must_use]
pub fn is_sticky_piston(state: &str) -> bool {
    redstone::base_name(state) == STICKY_PISTON
}

/// Whether `state` is a piston head.
#[must_use]
pub fn is_piston_head(state: &str) -> bool {
    redstone::base_name(state) == PISTON_HEAD
}

/// `PistonStructureResolver.isSticky`: slime or honey. Note **not** the piston's
/// own stickiness — this is about the *pushed* block dragging its neighbours.
#[must_use]
pub fn is_sticky_block(state: &str) -> bool {
    matches!(redstone::base_name(state), SLIME_BLOCK | HONEY_BLOCK)
}

/// `PistonStructureResolver.canStickToEachOther`: slime and honey each stick to
/// everything sticky **except each other**. The asymmetric-looking pair of early
/// returns in vanilla is symmetric in effect, and both orders are checked here for
/// the same reason vanilla writes both.
#[must_use]
pub fn can_stick_to_each_other(a: &str, b: &str) -> bool {
    let (a, b) = (redstone::base_name(a), redstone::base_name(b));
    if (a == HONEY_BLOCK && b == SLIME_BLOCK) || (a == SLIME_BLOCK && b == HONEY_BLOCK) {
        return false;
    }
    is_sticky_block(a) || is_sticky_block(b)
}

/// The `facing` property of a piston, head or moving piston. `Up` for a state
/// carrying none, matching every other `*_facing` reader in this family.
#[must_use]
pub fn piston_facing(state: &str) -> Direction {
    redstone::diode_facing(state)
}

/// The `extended` property of a piston base.
#[must_use]
pub fn piston_extended(state: &str) -> bool {
    state.contains("extended=true")
}

/// `PistonBaseBlock.isPushable`.
///
/// `allow_destroyable` is vanilla's own parameter name: the *first* block of a run
/// may be destroyed, blocks further along may not, and passing the wrong one is a
/// piston that eats a torch it should have stopped at.
///
/// The four hard-coded names are exceptions in `isPushable` itself, not
/// [`PushReaction::Block`] entries — reproducing them as table rows would make
/// obsidian unpushable *and* claim vanilla says so, which it does not.
#[must_use]
pub fn is_pushable(
    state: &str,
    direction: Direction,
    allow_destroyable: bool,
    connection_direction: Direction,
) -> bool {
    let base = redstone::base_name(state);
    if base == "minecraft:air" {
        return true;
    }
    if matches!(
        base,
        "minecraft:obsidian"
            | "minecraft:crying_obsidian"
            | "minecraft:respawn_anchor"
            | "minecraft:reinforced_deepslate"
    ) {
        return false;
    }
    if is_piston(state) {
        // A piston base is pushable only while retracted.
        return !piston_extended(state);
    }
    match push_reaction(state) {
        PushReaction::Block => false,
        PushReaction::Destroy => allow_destroyable,
        PushReaction::PushOnly => direction == connection_direction,
        // `!state.hasBlockEntity()`: a chest or a furnace is never pushed, read
        // off the real per-state census rather than a hand-kept name list.
        PushReaction::Normal => !has_block_entity(state),
    }
}

/// Whether a state carries a block entity, so a piston refuses to push it —
/// vanilla's final `!state.hasBlockEntity()`.
///
/// A state the 26.2 census cannot resolve reads as "no block entity", i.e.
/// pushable. That is the same direction every other unresolvable-state fallback in
/// this family takes, and the alternative — refusing to push an unknown block —
/// would silently freeze contraptions after a version bump.
fn has_block_entity(state: &str) -> bool {
    crate::mobs::block_state_id_or_default(state)
        .and_then(lodestone_data::block_entity_types::block_entity_type)
        .is_some()
}

// --- the structure resolver -----------------------------------------------

/// What one piston movement resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// Blocks to move, **in the order the resolver produced them**. The order is
    /// the behaviour: `apply_move` walks it backwards, and a collision reorder
    /// changes which block lands where.
    pub to_push: Vec<BlockPos>,
    /// Blocks to break and drop (a `DESTROY` block at the head of the run).
    pub to_destroy: Vec<BlockPos>,
    /// The direction blocks actually travel — the piston's facing when extending,
    /// its opposite when retracting.
    pub push_direction: Direction,
}

/// `PistonStructureResolver.resolve`. `None` is vanilla's `false`: the piston does
/// not move at all.
#[must_use]
pub fn resolve<F>(
    lookup: &F,
    piston_pos: BlockPos,
    direction: Direction,
    extending: bool,
) -> Option<Resolution>
where
    F: Fn(BlockPos) -> String,
{
    let push_direction = if extending { direction } else { direction.opposite() };
    let start_pos = if extending {
        direction.relative(piston_pos)
    } else {
        direction.relative(direction.relative(piston_pos))
    };

    // **The head is gone before the resolver runs.** Vanilla's `moveBlocks` sets the
    // arm cell to air *before* constructing the `PistonStructureResolver` when
    // retracting, so the resolver never sees `piston_head` — which is a `BLOCK`
    // push reaction and would refuse the whole pull. Reproduced by masking the cell
    // rather than by special-casing `piston_head` inside `is_pushable`, because
    // `piston_head` really is `BLOCK` and a passing piston head really does stop a
    // push.
    let arm_pos = direction.relative(piston_pos);
    let masked = |p: BlockPos| -> String {
        if !extending && p == arm_pos {
            let state = lookup(p);
            if is_piston_head(&state) {
                return "minecraft:air".to_string();
            }
        }
        lookup(p)
    };
    let lookup = &masked;

    let mut r = Resolver {
        lookup,
        piston_pos,
        piston_direction: direction,
        push_direction,
        to_push: Vec::new(),
        to_destroy: Vec::new(),
    };

    let start_state = (r.lookup)(start_pos);
    if !is_pushable(&start_state, push_direction, false, direction) {
        if extending && push_reaction(&start_state) == PushReaction::Destroy {
            return Some(Resolution {
                to_push: Vec::new(),
                to_destroy: vec![start_pos],
                push_direction,
            });
        }
        return None;
    }

    if !r.add_block_line(start_pos, push_direction) {
        return None;
    }
    // Indexed rather than iterated: `add_branching_blocks` appends to `to_push`
    // while this walks it, which is exactly how a sticky run grows sideways.
    let mut i = 0;
    while i < r.to_push.len() {
        let pos = r.to_push[i];
        if is_sticky_block(&(r.lookup)(pos)) && !r.add_branching_blocks(pos) {
            return None;
        }
        i += 1;
    }

    Some(Resolution {
        to_push: r.to_push,
        to_destroy: r.to_destroy,
        push_direction,
    })
}

struct Resolver<'a, F> {
    lookup: &'a F,
    piston_pos: BlockPos,
    piston_direction: Direction,
    push_direction: Direction,
    to_push: Vec<BlockPos>,
    to_destroy: Vec<BlockPos>,
}

impl<F> Resolver<'_, F>
where
    F: Fn(BlockPos) -> String,
{
    /// `PistonStructureResolver.addBlockLine`.
    fn add_block_line(&mut self, start: BlockPos, direction: Direction) -> bool {
        let mut next_state = (self.lookup)(start);
        if redstone::base_name(&next_state) == "minecraft:air" {
            return true;
        }
        if !is_pushable(&next_state, self.push_direction, false, direction) {
            return true;
        }
        if start == self.piston_pos {
            return true;
        }
        if self.to_push.contains(&start) {
            return true;
        }

        let mut block_count = 1usize;
        if block_count + self.to_push.len() > MAX_PUSH_DEPTH {
            return false;
        }

        // The sticky *backwards* run: a slime block drags whatever is behind it.
        while is_sticky_block(&next_state) {
            let pos = relative_n(start, self.push_direction.opposite(), block_count as i32);
            let previous_state = next_state.clone();
            next_state = (self.lookup)(pos);
            if redstone::base_name(&next_state) == "minecraft:air"
                || !can_stick_to_each_other(&previous_state, &next_state)
                || !is_pushable(
                    &next_state,
                    self.push_direction,
                    false,
                    self.push_direction.opposite(),
                )
                || pos == self.piston_pos
            {
                break;
            }
            block_count += 1;
            if block_count + self.to_push.len() > MAX_PUSH_DEPTH {
                return false;
            }
        }

        let mut blocks_added = 0usize;
        for i in (0..block_count).rev() {
            self.to_push
                .push(relative_n(start, self.push_direction.opposite(), i as i32));
            blocks_added += 1;
        }

        let mut i = 1i32;
        loop {
            let pos = relative_n(start, self.push_direction, i);
            if let Some(collision_pos) = self.to_push.iter().position(|p| *p == pos) {
                self.reorder_at_collision(blocks_added, collision_pos);
                for j in 0..=collision_pos + blocks_added {
                    let block_pos = self.to_push[j];
                    if is_sticky_block(&(self.lookup)(block_pos))
                        && !self.add_branching_blocks(block_pos)
                    {
                        return false;
                    }
                }
                return true;
            }

            let state = (self.lookup)(pos);
            if redstone::base_name(&state) == "minecraft:air" {
                return true;
            }
            if !is_pushable(&state, self.push_direction, true, self.push_direction)
                || pos == self.piston_pos
            {
                return false;
            }
            if push_reaction(&state) == PushReaction::Destroy {
                self.to_destroy.push(pos);
                return true;
            }
            if self.to_push.len() >= MAX_PUSH_DEPTH {
                return false;
            }
            self.to_push.push(pos);
            blocks_added += 1;
            i += 1;
        }
    }

    /// `PistonStructureResolver.reorderListAtCollision` — three splices, in this
    /// order. **Do not simplify.** The whole point is that the line added last
    /// jumps ahead of the line it collided with, and any other arrangement moves a
    /// different block into the vacated cell.
    fn reorder_at_collision(&mut self, blocks_added: usize, collision_pos: usize) {
        let len = self.to_push.len();
        let head: Vec<BlockPos> = self.to_push[..collision_pos].to_vec();
        let last_line_added: Vec<BlockPos> = self.to_push[len - blocks_added..].to_vec();
        let collision_to_line: Vec<BlockPos> =
            self.to_push[collision_pos..len - blocks_added].to_vec();
        self.to_push.clear();
        self.to_push.extend(head);
        self.to_push.extend(last_line_added);
        self.to_push.extend(collision_to_line);
    }

    /// `PistonStructureResolver.addBranchingBlocks`: a sticky block drags its
    /// perpendicular neighbours. Only perpendicular — the push axis is already
    /// covered by the line walk.
    fn add_branching_blocks(&mut self, from_pos: BlockPos) -> bool {
        let from_state = (self.lookup)(from_pos);
        for direction in ALL_DIRECTIONS {
            if axis_of(direction) == axis_of(self.push_direction) {
                continue;
            }
            let neighbour_pos = direction.relative(from_pos);
            let neighbour_state = (self.lookup)(neighbour_pos);
            if can_stick_to_each_other(&neighbour_state, &from_state)
                && !self.add_block_line(neighbour_pos, direction)
            {
                return false;
            }
        }
        let _ = self.piston_direction;
        true
    }
}

/// `BlockPos.relative(direction, n)`.
#[must_use]
pub fn relative_n(pos: BlockPos, direction: Direction, n: i32) -> BlockPos {
    let (dx, dy, dz) = match direction {
        Direction::Down => (0, -1, 0),
        Direction::Up => (0, 1, 0),
        Direction::North => (0, 0, -1),
        Direction::South => (0, 0, 1),
        Direction::West => (-1, 0, 0),
        Direction::East => (1, 0, 0),
    };
    BlockPos::new(pos.x + dx * n, pos.y + dy * n, pos.z + dz * n)
}

/// `Direction.getAxis()`, as the three characters `x`/`y`/`z`.
fn axis_of(direction: Direction) -> char {
    match direction {
        Direction::Down | Direction::Up => 'y',
        Direction::North | Direction::South => 'z',
        Direction::West | Direction::East => 'x',
    }
}

// --- the signal rule (quasi-connectivity) ---------------------------------

/// `PistonBaseBlock.getNeighborSignal` — **including quasi-connectivity**.
///
/// Three blocks, in vanilla's order:
///
/// 1. every direction except the push direction, read at the neighbour cell;
/// 2. the piston's own cell from `DOWN`;
/// 3. **`pos.above()`**, every direction except `DOWN`.
///
/// Step 3 is quasi-connectivity: a signal one block above the piston, touching
/// nothing else, extends it. It looks like a bug and is load-bearing — every BUD
/// switch and observer clock in the game is built on it, and a port that "fixes"
/// it silently loses half the contraptions people build.
///
/// Step 1's exclusion of the push direction is why a piston is not powered by the
/// block it is about to push.
#[must_use]
pub fn has_extend_signal<F>(lookup: &F, pos: BlockPos, push_direction: Direction) -> bool
where
    F: Fn(BlockPos) -> String,
{
    for direction in ALL_DIRECTIONS {
        if direction != push_direction
            && redstone::signal_at(lookup, direction.relative(pos), direction, false) > 0
        {
            return true;
        }
    }
    if redstone::signal_at(lookup, pos, Direction::Down, false) > 0 {
        return true;
    }
    let above = Direction::Up.relative(pos);
    for direction in ALL_DIRECTIONS {
        if direction != Direction::Down
            && redstone::signal_at(lookup, direction.relative(above), direction, false) > 0
        {
            return true;
        }
    }
    false
}

// --- applying a move ------------------------------------------------------

/// One cell this move rewrites: `(pos, new_state)`. `to` is the *final* state, so a
/// caller applies these in order and publishes each as an ordinary block change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveWrite {
    /// Where.
    pub pos: BlockPos,
    /// The state that ends up there.
    pub to: String,
}

/// Turns a [`Resolution`] into the final cell writes, for a piston at
/// `piston_pos` facing `direction`.
///
/// `extending` selects the direction blocks travel and whether a head is placed.
/// **This is the one-step form of vanilla's two-phase move** — see the module doc
/// for what that costs. The write order is: destroyed cells to air, then each
/// pushed block into its destination walking `to_push` **backwards** (so a run
/// never overwrites a block it has not moved yet), then the vacated cells to air,
/// then the head.
#[must_use]
pub fn apply_move<F>(
    lookup: &F,
    resolution: &Resolution,
    piston_pos: BlockPos,
    direction: Direction,
    extending: bool,
    sticky: bool,
) -> Vec<MoveWrite>
where
    F: Fn(BlockPos) -> String,
{
    let mut writes = Vec::new();
    let air = "minecraft:air".to_string();
    let arm_pos = direction.relative(piston_pos);

    for pos in &resolution.to_destroy {
        writes.push(MoveWrite { pos: *pos, to: air.clone() });
    }

    // Backwards, exactly as vanilla iterates: the far end of the run moves first,
    // so nothing is written over a block still waiting to move.
    let mut vacated: Vec<BlockPos> = Vec::new();
    for pos in resolution.to_push.iter().rev() {
        let state = (lookup)(*pos);
        let target = resolution.push_direction.relative(*pos);
        writes.push(MoveWrite { pos: target, to: state });
        vacated.push(*pos);
    }

    // A cell that something else moved *into* is not vacated — and on extension the
    // arm cell is about to hold the head, so writing air there first would publish
    // two changes for one cell.
    let occupied: Vec<BlockPos> = writes.iter().map(|w| w.pos).collect();
    for pos in vacated {
        if occupied.contains(&pos) || (extending && pos == arm_pos) {
            continue;
        }
        writes.push(MoveWrite { pos, to: air.clone() });
    }

    if extending {
        let kind = if sticky { "sticky" } else { "normal" };
        writes.push(MoveWrite {
            pos: arm_pos,
            to: format!(
                "{PISTON_HEAD}[facing={},short=false,type={kind}]",
                facing_name(direction)
            ),
        });
    } else if !occupied.contains(&arm_pos) {
        // The head cell empties on retraction unless the pulled run filled it.
        writes.push(MoveWrite { pos: arm_pos, to: air });
    }

    writes
}

/// The `facing` property value for a direction.
#[must_use]
pub fn facing_name(direction: Direction) -> &'static str {
    match direction {
        Direction::Down => "down",
        Direction::Up => "up",
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
    }
}

/// [`facing_name`]'s inverse, for reading a direction back out of a
/// [`finish_kind`] string.
#[must_use]
pub fn direction_named(name: &str) -> Option<Direction> {
    Some(match name {
        "down" => Direction::Down,
        "up" => Direction::Up,
        "north" => Direction::North,
        "south" => Direction::South,
        "west" => Direction::West,
        "east" => Direction::East,
        _ => return None,
    })
}

// --- the two-phase move ---------------------------------------------------

/// `minecraft:moving_piston` — the block that holds a travelling cell for the
/// duration of a move. Its own render shape is `INVISIBLE`; everything a client
/// draws there comes from the block entity below.
pub const MOVING_PISTON: &str = "minecraft:moving_piston";

/// The `minecraft:block_entity_type` key `moving_piston` owns
/// (`MovingPistonBlock.getTicker` names `BlockEntityTypes.PISTON`). **It is not
/// `minecraft:moving_piston`** — the block and its block entity have different
/// registry keys, and sending the block's name as the type id resolves to some
/// other entity or to nothing.
pub const PISTON_BLOCK_ENTITY: &str = "minecraft:piston";

/// `PistonMovingBlockEntity.tick`'s `progress += 0.5F`, so a whole travel is two
/// ticks of ramp.
///
/// The server does **not** stream this: `saveAdditional` writes `progressO`, the
/// value at the *start* of the tick, and the wire carries it once. A client runs
/// the ramp itself, which is why getting the seed and the two tick numbers below
/// right is the whole of the server's job here.
pub const PISTON_PROGRESS_SPEED: f32 = 0.5;

/// The `progress` a freshly created moving block entity reports —
/// `progressO`, which is `0.0` before the first tick advances it.
pub const PISTON_INITIAL_PROGRESS: f32 = 0.0;

/// How many ticks after the push tick a move commits: **two**.
///
/// Derived from `ServerLevel.tick`'s own ordering rather than guessed.
/// `runBlockEvents` runs *before* `tickBlockEntities`, and
/// `Level.addBlockEntityTicker` appends straight to the live list when the tick
/// loop is not already inside it — so the block entity `moveBlocks` creates on
/// tick `N` is ticked on tick `N` too. `PistonMovingBlockEntity.tick` then reads
/// `progressO` and takes the ramp branch while it is below `1.0`:
///
/// | tick | `progressO` at entry | `progress` at exit | branch |
/// |---|---|---|---|
/// | `N` | 0.0 | 0.5 | ramp |
/// | `N + 1` | 0.5 | 1.0 | ramp |
/// | `N + 2` | 1.0 | 1.0 | **commit** — the block entity is removed and `movedState` is written |
///
/// So the entity is alive for three ticks and the world commits on `N + 2`. A
/// delay of 1 would halve the animation; a delay of 3 would hold the cells empty
/// for an extra tick.
pub const PISTON_MOVE_DELAY: u64 = 2;

/// The scheduled-tick kind prefix a pending commit runs under, in the same
/// namespace as [`TICK_PISTON`] and [`redstone::TICK_REPEATER`].
///
/// A full kind carries the whole block entity after this prefix — see
/// [`finish_kind`] — so a kind must be matched with [`is_finish_kind`] rather
/// than compared for equality.
pub const TICK_PISTON_FINISH: &str = "redstone:piston_finish";

/// One in-flight `PistonMovingBlockEntity`: the four fields that decide both what
/// a client draws and what the world commits.
///
/// `progress` is deliberately absent. It is always [`PISTON_INITIAL_PROGRESS`] at
/// creation and the client owns the ramp from there, so storing it would be a
/// second source of truth for a value nothing here ever changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovingBlockEntity {
    /// `blockState` — the state travelling through this cell, and exactly what
    /// lands here when the move commits.
    pub moved_state: String,
    /// `facing` — the **piston's** facing, not the direction blocks travel. The
    /// two differ on every retraction, and a client derives the travel direction
    /// itself from `facing` plus `extending`
    /// (`PistonMovingBlockEntity.getMovementDirection`).
    pub direction: Direction,
    /// `extending`. Also selects the sign of `getExtendedProgress`
    /// (`extending ? p - 1 : 1 - p`).
    pub extending: bool,
    /// `source` (`isSourcePiston`) — whether this cell belongs to the piston
    /// itself rather than to a block being carried. True for the arm cell of an
    /// extension (which carries the head) and for the base cell of a retraction
    /// (which carries the base, and is what makes a client draw the head coming
    /// home).
    pub source: bool,
}

impl MovingBlockEntity {
    /// `Direction.get3DDataValue()` — the byte `Direction.LEGACY_ID_CODEC` stores
    /// for `facing`.
    ///
    /// The order is vanilla's own enum declaration order, which is neither
    /// alphabetical nor the horizontal-facing order every `*_facing` property
    /// uses. It is a **byte** on the wire, not an int.
    /// This entity's `getUpdateTag` payload — see
    /// [`crate::block_entities::moving_piston_nbt`], which owns the port of
    /// `saveAdditional`. Here so a caller outside this crate can reach it:
    /// `block_entities` is crate-private and this type is not.
    #[must_use]
    pub fn update_tag(&self) -> lodestone_core::Nbt {
        crate::block_entities::moving_piston_nbt(self)
    }

    #[must_use]
    pub fn facing_3d_value(&self) -> i8 {
        match self.direction {
            Direction::Down => 0,
            Direction::Up => 1,
            Direction::North => 2,
            Direction::South => 3,
            Direction::West => 4,
            Direction::East => 5,
        }
    }
}

/// The `moving_piston` block state for a cell.
///
/// `sticky` is the *piston's* stickiness and applies only to a `source` cell:
/// `moveBlocks` writes plain `Blocks.MOVING_PISTON.defaultBlockState()` — i.e.
/// `type=normal` — for every carried block, and sets `TYPE` from the piston only
/// for the arm cell it creates itself. Nothing a client draws reads this
/// property (it takes `type` from the *moved* state), so a wrong value here is
/// invisible; it is reproduced because the block state is also what a chunk save
/// and a neighbour query see.
#[must_use]
pub fn moving_piston_state(direction: Direction, sticky: bool) -> String {
    format!(
        "{MOVING_PISTON}[facing={},type={}]",
        facing_name(direction),
        if sticky { "sticky" } else { "normal" }
    )
}

/// Whether `state` is a `moving_piston`.
#[must_use]
pub fn is_moving_piston(state: &str) -> bool {
    redstone::base_name(state) == MOVING_PISTON
}

/// Serialises a [`MovingBlockEntity`] into a scheduled-tick kind.
///
/// **The pending commit tick *is* this crate's `PistonMovingBlockEntity`.** There
/// is no per-position block-entity map on the path a piston move runs on
/// (`crate::random_tick`'s reaction surface holds a `ChunkColumn` and a
/// [`crate::scheduled_tick::ScheduledTickQueue`] and nothing else), and the queue
/// is keyed by position with a free-form payload — so the queue entry carries the
/// record, and "read the moving block entity at this cell" is "find the pending
/// commit at this cell". [`parse_finish_kind`] is the other half.
///
/// `|` is the separator because a canonical block-state string can contain
/// `:`, `[`, `]`, `,` and `=` but never a pipe.
#[must_use]
pub fn finish_kind(entity: &MovingBlockEntity) -> String {
    format!(
        "{TICK_PISTON_FINISH}|{}|{}|{}|{}",
        facing_name(entity.direction),
        entity.extending,
        entity.source,
        entity.moved_state
    )
}

/// Whether `kind` is a pending piston commit. A prefix test, not an equality
/// test, because [`finish_kind`] appends the record.
#[must_use]
pub fn is_finish_kind(kind: &str) -> bool {
    kind.starts_with(TICK_PISTON_FINISH)
}

/// [`finish_kind`]'s inverse. `None` for any kind this did not write.
///
/// `splitn(4, '|')` rather than `split`: the moved state is the last field and
/// must be taken whole even though it is the only one that could ever contain a
/// separator, which it cannot.
#[must_use]
pub fn parse_finish_kind(kind: &str) -> Option<MovingBlockEntity> {
    let rest = kind.strip_prefix(TICK_PISTON_FINISH)?.strip_prefix('|')?;
    let mut parts = rest.splitn(4, '|');
    let direction = direction_named(parts.next()?)?;
    let extending = parse_bool(parts.next()?)?;
    let source = parse_bool(parts.next()?)?;
    let moved_state = parts.next()?;
    if moved_state.is_empty() {
        return None;
    }
    Some(MovingBlockEntity {
        moved_state: moved_state.to_string(),
        direction,
        extending,
        source,
    })
}

fn parse_bool(text: &str) -> Option<bool> {
    match text {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// The first phase of a move: what the world looks like *during* the animation.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MoveStart {
    /// Cells that hold a `moving_piston` for [`PISTON_MOVE_DELAY`] ticks:
    /// `(cell, the block state to write there, the block entity it carries)`.
    ///
    /// A caller writes the state, publishes the entity, and schedules
    /// [`finish_kind`] at `current_tick + PISTON_MOVE_DELAY`.
    pub moving: Vec<(BlockPos, String, MovingBlockEntity)>,
    /// Cells that become air on the push tick — the run's vacated tail and any
    /// destroyed block. Vanilla's `deleteAfterMove` loop uses flag 82, whose
    /// `UPDATE_CLIENTS` bit is set, so these are visible immediately; only the
    /// cells that receive something are deferred.
    pub cleared: Vec<BlockPos>,
    /// The piston base's own immediate write, when the base changes on the push
    /// tick rather than on the commit tick.
    ///
    /// `Some` on extension (`setBlock(pos, extendedState, 67)` in
    /// `PistonBaseBlock.triggerEvent`, immediate and client-visible) and `None`
    /// on retraction, where the base cell is itself one of [`moving`](Self::moving)
    /// — that is what animates the head coming home.
    pub base_now: Option<String>,
}

/// Splits [`apply_move`]'s one-step writes into the two phases vanilla performs.
///
/// **Derived from `apply_move`'s output rather than recomputed**, which is what
/// makes the second phase byte-identical to the one-step path by construction: a
/// write whose target is air happens now, and every other write is deferred
/// behind a `moving_piston` whose block entity carries that exact state.
/// [`finish_move`] then replays them.
///
/// `piston_state` is the base's state *before* the move — its stickiness and its
/// other properties are read off it, so a caller must not have rewritten the base
/// cell yet.
#[must_use]
pub fn begin_move(
    writes: &[MoveWrite],
    piston_state: &str,
    piston_pos: BlockPos,
    direction: Direction,
    extending: bool,
) -> MoveStart {
    let sticky = is_sticky_piston(piston_state);
    let arm_pos = direction.relative(piston_pos);
    let mut start = MoveStart::default();

    for write in writes {
        if redstone::base_name(&write.to) == "minecraft:air" {
            start.cleared.push(write.pos);
            continue;
        }
        // Only the arm cell of an extension is the piston's own: `moveBlocks`
        // creates it with `isSourcePiston = true` and the head as its moved
        // state, while every carried block gets `false`.
        let source = extending && write.pos == arm_pos;
        start.moving.push((
            write.pos,
            moving_piston_state(direction, source && sticky),
            MovingBlockEntity {
                moved_state: write.to.clone(),
                direction,
                extending,
                source,
            },
        ));
    }

    if extending {
        start.base_now = Some(redstone::with_property(piston_state, "extended", "true"));
    } else {
        // `PistonBaseBlock.triggerEvent`'s contract arm: the base cell itself
        // becomes a `moving_piston` carrying the *base* block, which is the only
        // record a client can draw a retracting head from
        // (`PistonHeadRenderer`'s `isSourcePiston && !isExtending` arm builds the
        // head from the base's own `facing` and stickiness).
        start.moving.push((
            piston_pos,
            moving_piston_state(direction, sticky),
            MovingBlockEntity {
                moved_state: redstone::with_property(piston_state, "extended", "false"),
                direction,
                extending: false,
                source: true,
            },
        ));
    }

    start
}

/// The second phase: the writes a commit performs, `PISTON_MOVE_DELAY` ticks
/// after [`begin_move`].
///
/// Each cell commits *itself* from its own block entity — vanilla's
/// `PistonMovingBlockEntity.tick` writes `entity.movedState` with no reference to
/// the piston that started the move, which is why a caller can schedule one
/// independent tick per cell rather than replaying the whole move.
///
/// Note this is the `tick` branch, **not** `finalTick`: `finalTick`'s
/// `isSourcePiston` arm writes air, and it is reached only when a piston is
/// interrupted mid-animation, never on normal completion. Using it here would
/// delete the head of every extension.
#[must_use]
pub fn finish_move(start: &MoveStart) -> Vec<MoveWrite> {
    start
        .moving
        .iter()
        .map(|(pos, _, entity)| MoveWrite {
            pos: *pos,
            to: entity.moved_state.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake world: an explicit position→state map, air everywhere else — the same
    /// "pure decision, fake world via closure" shape `crate::redstone`'s own tests
    /// use.
    fn world(entries: &[(BlockPos, &str)]) -> impl Fn(BlockPos) -> String + use<> {
        let entries: Vec<(BlockPos, String)> =
            entries.iter().map(|(p, s)| (*p, (*s).to_string())).collect();
        move |p: BlockPos| {
            entries
                .iter()
                .find(|(pos, _)| *pos == p)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| "minecraft:air".to_string())
        }
    }

    fn at(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    /// The push-reaction table is sorted (both name lists are binary-searched) and
    /// carries the counts `Blocks.java` actually declares. A table that silently
    /// lost rows makes a piston eat a torch it should stop at.
    #[test]
    fn push_reaction_table_matches_blocks_java() {
        assert!(DESTROY.windows(2).all(|w| w[0] < w[1]), "DESTROY must be sorted");
        assert!(BLOCKED.windows(2).all(|w| w[0] < w[1]), "BLOCKED must be sorted");
        assert_eq!(DESTROY.len(), 200);
        assert_eq!(BLOCKED.len(), 11);
        assert_eq!(PUSH_ONLY.len(), 16);

        assert_eq!(push_reaction("minecraft:torch"), PushReaction::Destroy);
        assert_eq!(push_reaction("minecraft:anvil"), PushReaction::Block);
        assert_eq!(
            push_reaction("minecraft:white_glazed_terracotta"),
            PushReaction::PushOnly
        );
        assert_eq!(push_reaction("minecraft:stone"), PushReaction::Normal);
        // The four `isPushable` exceptions are *not* table rows — asserting that
        // keeps the distinction the doc comment claims.
        assert_eq!(push_reaction("minecraft:obsidian"), PushReaction::Normal);
        assert!(!is_pushable(
            "minecraft:obsidian",
            Direction::East,
            false,
            Direction::East
        ));
        // A chest has a block entity, so it is never pushed even though its
        // reaction is NORMAL.
        assert_eq!(push_reaction("minecraft:chest"), PushReaction::Normal);
        assert!(!is_pushable(
            "minecraft:chest[facing=north,type=single,waterlogged=false]",
            Direction::East,
            false,
            Direction::East
        ));
        // Glazed terracotta moves along the push axis and refuses sideways drag.
        assert!(is_pushable(
            "minecraft:white_glazed_terracotta",
            Direction::East,
            false,
            Direction::East
        ));
        assert!(!is_pushable(
            "minecraft:white_glazed_terracotta",
            Direction::East,
            false,
            Direction::North
        ));
    }

    /// The 12-block limit, and that the thirteenth block refuses the whole move
    /// rather than pushing twelve of them.
    #[test]
    fn twelve_pushes_and_thirteen_refuses() {
        let mut twelve: Vec<(BlockPos, &str)> = vec![(at(0, 0, 0), PISTON)];
        for i in 1..=12 {
            twelve.push((at(i, 0, 0), "minecraft:stone"));
        }
        let resolved = resolve(&world(&twelve), at(0, 0, 0), Direction::East, true)
            .expect("twelve blocks is exactly the limit");
        assert_eq!(resolved.to_push.len(), 12);

        let mut thirteen = twelve.clone();
        thirteen.push((at(13, 0, 0), "minecraft:stone"));
        assert!(
            resolve(&world(&thirteen), at(0, 0, 0), Direction::East, true).is_none(),
            "a thirteenth block must refuse the move outright, not push twelve"
        );
    }

    /// A `DESTROY` block at the head of a run is destroyed, not pushed; the blocks
    /// behind it still move.
    #[test]
    fn a_destroy_block_at_the_head_is_destroyed() {
        let w = world(&[
            (at(0, 0, 0), PISTON),
            (at(1, 0, 0), "minecraft:stone"),
            (at(2, 0, 0), "minecraft:torch"),
        ]);
        let resolved = resolve(&w, at(0, 0, 0), Direction::East, true).expect("resolves");
        assert_eq!(resolved.to_push, vec![at(1, 0, 0)]);
        assert_eq!(resolved.to_destroy, vec![at(2, 0, 0)]);
    }

    /// **Slime drags perpendicular neighbours; honey and slime do not stick to each
    /// other.** The second half is the asymmetry a "sticky means sticky" shortcut
    /// gets wrong, and it changes which blocks a contraption carries.
    #[test]
    fn sticky_branching_and_the_slime_honey_exception() {
        let w = world(&[
            (at(0, 0, 0), PISTON),
            (at(1, 0, 0), SLIME_BLOCK),
            // Perpendicular to the push axis, so it is dragged along.
            (at(1, 1, 0), "minecraft:stone"),
        ]);
        let resolved = resolve(&w, at(0, 0, 0), Direction::East, true).expect("resolves");
        assert!(
            resolved.to_push.contains(&at(1, 1, 0)),
            "slime must drag its perpendicular neighbour; got {:?}",
            resolved.to_push
        );

        assert!(can_stick_to_each_other(SLIME_BLOCK, "minecraft:stone"));
        assert!(can_stick_to_each_other(HONEY_BLOCK, "minecraft:stone"));
        assert!(
            !can_stick_to_each_other(SLIME_BLOCK, HONEY_BLOCK),
            "slime and honey must not stick to each other"
        );
        assert!(
            !can_stick_to_each_other(HONEY_BLOCK, SLIME_BLOCK),
            "and not in the other order either"
        );

        let w = world(&[
            (at(0, 0, 0), PISTON),
            (at(1, 0, 0), SLIME_BLOCK),
            (at(1, 1, 0), HONEY_BLOCK),
        ]);
        let resolved = resolve(&w, at(0, 0, 0), Direction::East, true).expect("resolves");
        assert!(
            !resolved.to_push.contains(&at(1, 1, 0)),
            "honey must not be dragged by slime; got {:?}",
            resolved.to_push
        );
    }

    /// **A retraction resolves from two blocks out, not one.** `startPos` is
    /// `pistonPos.relative(direction, 2)` when retracting, because the head
    /// occupies the cell in between — getting this off by one is a sticky piston
    /// that pulls its own head instead of the block behind it.
    #[test]
    fn retraction_starts_two_blocks_out() {
        let w = world(&[
            (at(0, 0, 0), "minecraft:sticky_piston[extended=true,facing=east]"),
            (at(1, 0, 0), "minecraft:piston_head[facing=east,short=false,type=sticky]"),
            (at(2, 0, 0), SLIME_BLOCK),
        ]);
        let resolved = resolve(&w, at(0, 0, 0), Direction::East, false).expect("resolves");
        assert_eq!(resolved.push_direction, Direction::West);
        assert_eq!(resolved.to_push, vec![at(2, 0, 0)]);
    }

    /// **Quasi-connectivity.** A signal one block above the piston, touching
    /// nothing else, extends it — and the block the piston is about to push does
    /// *not* power it.
    #[test]
    fn quasi_connectivity_and_the_push_direction_exclusion() {
        // A lit torch beside the cell *above* the piston. It touches no face of the
        // piston itself — `(1, 1, 0)` is diagonal from `(0, 0, 0)` — so only the
        // `pos.above()` loop can see it. That is quasi-connectivity, and it is the
        // whole reason a BUD switch works.
        let above = world(&[
            (at(0, 0, 0), "minecraft:piston[extended=false,facing=east]"),
            (at(1, 1, 0), "minecraft:redstone_torch[lit=true]"),
        ]);
        assert!(
            has_extend_signal(&above, at(0, 0, 0), Direction::East),
            "a signal one block above the piston must extend it (quasi-connectivity)"
        );

        // Directly adjacent, ordinary powering.
        let beside = world(&[
            (at(0, 0, 0), "minecraft:piston[extended=false,facing=east]"),
            (at(0, 0, 1), "minecraft:redstone_torch[lit=true]"),
        ]);
        assert!(has_extend_signal(&beside, at(0, 0, 0), Direction::East));

        // In the push direction: excluded, so the piston is not powered by what it
        // is about to push.
        let ahead = world(&[
            (at(0, 0, 0), "minecraft:piston[extended=false,facing=east]"),
            (at(1, 0, 0), "minecraft:redstone_torch[lit=true]"),
        ]);
        assert!(
            !has_extend_signal(&ahead, at(0, 0, 0), Direction::East),
            "the block being pushed must not power the piston"
        );
        // …and the same block does power a piston facing away from it.
        assert!(has_extend_signal(&ahead, at(0, 0, 0), Direction::West));

        // Nothing at all.
        let bare = world(&[(at(0, 0, 0), "minecraft:piston[extended=false,facing=east]")]);
        assert!(!has_extend_signal(&bare, at(0, 0, 0), Direction::East));
    }

    /// The writes an extension produces: the run shifted one cell forward, the
    /// vacated cell filled by the head, nothing left behind.
    #[test]
    fn extension_writes_shift_the_run_and_place_the_head() {
        let w = world(&[
            (at(0, 0, 0), "minecraft:piston[extended=false,facing=east]"),
            (at(1, 0, 0), "minecraft:stone"),
            (at(2, 0, 0), "minecraft:dirt"),
        ]);
        let resolved = resolve(&w, at(0, 0, 0), Direction::East, true).expect("resolves");
        assert_eq!(resolved.to_push, vec![at(1, 0, 0), at(2, 0, 0)]);

        let writes = apply_move(&w, &resolved, at(0, 0, 0), Direction::East, true, false);
        let find = |pos: BlockPos| {
            writes
                .iter()
                .find(|w| w.pos == pos)
                .map(|w| w.to.as_str())
                .unwrap_or("(unwritten)")
        };
        assert_eq!(find(at(3, 0, 0)), "minecraft:dirt", "the far block moves first");
        assert_eq!(find(at(2, 0, 0)), "minecraft:stone");
        assert_eq!(
            find(at(1, 0, 0)),
            "minecraft:piston_head[facing=east,short=false,type=normal]",
            "the head fills the cell the run vacated"
        );
        assert!(
            !writes.iter().any(|w| w.pos == at(0, 0, 0)),
            "the base cell is the caller's to rewrite, not `apply_move`'s"
        );
    }

    // --- the two-phase move -----------------------------------------------
    //
    // The outside expectation for everything below is `PistonBaseBlock.moveBlocks`
    // plus `PistonMovingBlockEntity.tick`, read as record definitions, and — for
    // the byte-identity gate — the *already verified* one-step path. Comparing the
    // animated path against behaviour that was gated before it existed is a
    // legitimate outside expectation; comparing it against a fresh guess would not
    // be.

    /// A mutable fake world. Air is represented by **absence**, so two snapshots
    /// compare equal exactly when every cell agrees — an explicit
    /// `"minecraft:air"` entry beside a missing one would read as a mismatch and
    /// make the byte-identity gate fail for a reason that is not about pistons.
    type FakeWorld = std::collections::BTreeMap<(i32, i32, i32), String>;

    fn fake(entries: &[(BlockPos, &str)]) -> FakeWorld {
        entries
            .iter()
            .map(|(p, s)| ((p.x, p.y, p.z), (*s).to_string()))
            .collect()
    }

    fn reader(w: &FakeWorld) -> impl Fn(BlockPos) -> String + '_ {
        move |p: BlockPos| {
            w.get(&(p.x, p.y, p.z))
                .cloned()
                .unwrap_or_else(|| "minecraft:air".to_string())
        }
    }

    fn put(w: &mut FakeWorld, pos: BlockPos, state: &str) {
        if redstone::base_name(state) == "minecraft:air" {
            w.remove(&(pos.x, pos.y, pos.z));
        } else {
            w.insert((pos.x, pos.y, pos.z), state.to_string());
        }
    }

    /// One move's `Resolution` and the one-step writes it produces, sharing the
    /// exact gating `crate::random_tick`'s piston arm applies (a retraction always
    /// happens; a normal piston does not pull).
    fn plan_move(
        w: &FakeWorld,
        piston: BlockPos,
        facing: Direction,
        extending: bool,
    ) -> Option<(String, Vec<MoveWrite>)> {
        let piston_state = reader(w)(piston);
        let sticky = is_sticky_piston(&piston_state);
        let resolution = resolve(&reader(w), piston, facing, extending);
        let resolution = match (extending, resolution) {
            (true, None) => return None,
            (_, Some(resolution)) => resolution,
            (false, None) => Resolution {
                to_push: Vec::new(),
                to_destroy: Vec::new(),
                push_direction: facing.opposite(),
            },
        };
        let resolution = if extending || sticky {
            resolution
        } else {
            Resolution { to_push: Vec::new(), ..resolution }
        };
        let writes = apply_move(&reader(w), &resolution, piston, facing, extending, sticky);
        Some((piston_state, writes))
    }

    /// The world the **one-step** path leaves behind: every write applied at once,
    /// then the base's `extended` flipped. This is the arm that was already gated
    /// before the animation existed, and it is the expectation the two-phase path
    /// has to land on.
    fn one_step_world(
        setup: &[(BlockPos, &str)],
        piston: BlockPos,
        facing: Direction,
        extending: bool,
    ) -> FakeWorld {
        let mut w = fake(setup);
        let (piston_state, writes) =
            plan_move(&w, piston, facing, extending).expect("the move must resolve");
        for write in &writes {
            put(&mut w, write.pos, &write.to);
        }
        let flag = if extending { "true" } else { "false" };
        let base = redstone::with_property(&piston_state, "extended", flag);
        put(&mut w, piston, &base);
        w
    }

    /// The world **during** the animation and the world after the commit.
    fn two_phase_worlds(
        setup: &[(BlockPos, &str)],
        piston: BlockPos,
        facing: Direction,
        extending: bool,
    ) -> (FakeWorld, FakeWorld, MoveStart) {
        let mut w = fake(setup);
        let (piston_state, writes) =
            plan_move(&w, piston, facing, extending).expect("the move must resolve");
        let start = begin_move(&writes, &piston_state, piston, facing, extending);
        for pos in &start.cleared {
            put(&mut w, *pos, "minecraft:air");
        }
        for (pos, moving_state, _) in &start.moving {
            put(&mut w, *pos, moving_state);
        }
        if let Some(base_now) = &start.base_now {
            put(&mut w, piston, base_now);
        }
        let mid = w.clone();
        for write in finish_move(&start) {
            put(&mut w, write.pos, &write.to);
        }
        (mid, w, start)
    }

    /// Every cell on which two worlds disagree, as
    /// `(pos, left, right)`. **Collected rather than asserted in the loop**: an
    /// `assert!` inside the walk would report one cell and leave the rest as
    /// argument, and the control below needs to know *how many* disagree.
    fn mismatches(
        left: &FakeWorld,
        right: &FakeWorld,
    ) -> Vec<((i32, i32, i32), Option<String>, Option<String>)> {
        let keys: std::collections::BTreeSet<(i32, i32, i32)> =
            left.keys().chain(right.keys()).copied().collect();
        keys.into_iter()
            .filter(|k| left.get(k) != right.get(k))
            .map(|k| (k, left.get(&k).cloned(), right.get(&k).cloned()))
            .collect()
    }

    /// The four shapes every gate below runs, as
    /// `(name, setup, piston, facing, extending)`.
    ///
    /// **A one-block push cannot separate the lifecycle from a snap**, so none of
    /// these is one: the shortest run here is two cells, and each shape exercises
    /// a different limb of `apply_move` (a plain run, a destroyed head, a sticky
    /// branch, a retraction whose moving cell is the base itself).
    ///
    /// The last field is the number of cells the move must **defer** behind a
    /// `moving_piston`, hand-derived from `apply_move`'s own write list per shape
    /// (destinations, plus the arm on an extension, plus the base on a retraction).
    /// It is stated here rather than read back off `begin_move` on purpose: a
    /// control whose expected count comes from the code under test is satisfied by
    /// a `begin_move` that defers nothing at all.
    #[allow(clippy::type_complexity)]
    fn scenarios(
    ) -> Vec<(&'static str, Vec<(BlockPos, &'static str)>, BlockPos, Direction, bool, usize)> {
        vec![
            (
                "three blocks pushed east",
                vec![
                    (at(0, 0, 0), "minecraft:piston[extended=false,facing=east]"),
                    (at(1, 0, 0), "minecraft:stone"),
                    (at(2, 0, 0), "minecraft:dirt"),
                    (at(3, 0, 0), "minecraft:gravel"),
                ],
                at(0, 0, 0),
                Direction::East,
                true,
                // three destinations (2,3,4) plus the arm cell (1).
                4,
            ),
            (
                "two blocks pushed into a torch that is destroyed",
                vec![
                    (at(0, 0, 0), "minecraft:piston[extended=false,facing=east]"),
                    (at(1, 0, 0), "minecraft:stone"),
                    (at(2, 0, 0), "minecraft:dirt"),
                    (at(3, 0, 0), "minecraft:torch"),
                ],
                at(0, 0, 0),
                Direction::East,
                true,
                // two destinations (2,3) plus the arm cell (1). Cell 3 is written
                // twice by `apply_move` — air for the destroyed torch, then the
                // block that lands on it — and must end up deferred, not empty.
                3,
            ),
            (
                "a sticky run dragging a perpendicular neighbour",
                vec![
                    (at(0, 0, 0), "minecraft:sticky_piston[extended=false,facing=east]"),
                    (at(1, 0, 0), SLIME_BLOCK),
                    (at(2, 0, 0), "minecraft:stone"),
                    (at(1, 1, 0), "minecraft:dirt"),
                ],
                at(0, 0, 0),
                Direction::East,
                true,
                // destinations (2,0,0), (3,0,0), (2,1,0) plus the arm cell (1,0,0);
                // the dragged neighbour's own cell (1,1,0) empties immediately.
                4,
            ),
            (
                "a sticky retraction pulling two blocks home",
                vec![
                    (at(0, 0, 0), "minecraft:sticky_piston[extended=true,facing=east]"),
                    (at(1, 0, 0), "minecraft:piston_head[facing=east,short=false,type=sticky]"),
                    (at(2, 0, 0), SLIME_BLOCK),
                    (at(3, 0, 0), "minecraft:stone"),
                ],
                at(0, 0, 0),
                Direction::East,
                false,
                // destinations (1,0,0) and (2,0,0), plus the **base** cell (0,0,0),
                // which is what animates on a retraction.
                3,
            ),
        ]
    }

    /// **The invariant that makes the animation safe to ship**: the world
    /// [`PISTON_MOVE_DELAY`] ticks after a push is identical, cell for cell, to the
    /// world the one-step path produced immediately.
    ///
    /// The control is the *same comparison against the mid-animation world*, which
    /// must fail — and it is not decoration. Without it this gate is satisfied by a
    /// [`begin_move`] that simply applies everything at once and a [`finish_move`]
    /// that writes nothing, which is precisely the bug it exists to forbid.
    #[test]
    fn the_two_phase_world_matches_the_one_step_path_cell_for_cell() {
        let mut failures: Vec<String> = Vec::new();
        for (name, setup, piston, facing, extending, deferred) in scenarios() {
            let one = one_step_world(&setup, piston, facing, extending);
            let (mid, two, start) = two_phase_worlds(&setup, piston, facing, extending);

            let after = mismatches(&one, &two);
            if !after.is_empty() {
                failures.push(format!(
                    "{name}: the committed world differs from the one-step world at \
                     {} cell(s): {after:?}",
                    after.len()
                ));
            }

            // Control. Every cell that is going to receive a block must hold
            // `moving_piston` mid-animation and *not* the final block — otherwise a
            // client has nothing left to animate, which is the same defect shape as
            // a correct packet sent in the wrong order. The count is predicted, not
            // merely required to be non-zero: it is exactly the number of cells
            // `begin_move` deferred.
            if start.moving.len() != deferred {
                failures.push(format!(
                    "{name}: expected {deferred} deferred cell(s), got {}",
                    start.moving.len()
                ));
            }
            let during = mismatches(&one, &mid);
            if during.len() != deferred {
                failures.push(format!(
                    "{name}: expected the mid-animation world to differ from the final \
                     world at exactly {deferred} deferred cell(s), got {}: {during:?}",
                    during.len()
                ));
            }
            for (pos, _, mid_state) in &during {
                let held = mid_state.as_deref().unwrap_or("(air)");
                if !is_moving_piston(held) {
                    failures.push(format!(
                        "{name}: cell {pos:?} differs mid-animation but holds {held}, \
                         not a moving_piston"
                    ));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    /// The commit is scheduled **exactly two ticks** after the push, driven through
    /// the real reaction surface and the real queue rather than by calling
    /// [`begin_move`] directly.
    ///
    /// The two hypotheses are separated by construction: a delay of 1 puts every
    /// commit on `PUSH + 1` and a delay of 2 puts it on `PUSH + 2`, so both of
    /// those ticks are asserted — the first empty, the second full. A gate that
    /// only checked "it eventually commits" passes under either.
    ///
    /// `PUSH` is 40 rather than 0 deliberately: at tick 0 an absolute due-tick and
    /// a relative delay are the same number, which is the coincidence that lets a
    /// rebasing bug through.
    #[test]
    fn a_push_schedules_its_commits_two_ticks_out_and_not_one() {
        use crate::chunk::ChunkColumn;
        use crate::scheduled_tick::ScheduledTickQueue;

        const PUSH: u64 = 40;

        let mut column = ChunkColumn::new(0, 16);
        // A piston at (4,5,4) facing east with three blocks in front of it, and a
        // lit redstone torch directly beside the base so `has_extend_signal` fires
        // on the ordinary adjacency path rather than through quasi-connectivity.
        column.set_block(4, 5, 4, "minecraft:piston[extended=false,facing=east]");
        column.set_block(5, 5, 4, "minecraft:stone");
        column.set_block(6, 5, 4, "minecraft:dirt");
        column.set_block(7, 5, 4, "minecraft:gravel");
        column.set_block(4, 5, 5, "minecraft:redstone_torch[lit=true]");

        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        let events =
            crate::random_tick::propagate_and_react(&mut column, 0, 0, 4, 5, 5, &mut queue, PUSH);

        // The push happened *now*: **four** cells hold moving_piston, not three.
        // The arm cell is one of them — the head is a travelling block like any
        // other, so it animates rather than appearing instantly. Predicting three
        // (one per pushed block) is the plausible-looking wrong answer, and this
        // gate failed on it first time.
        let moving_now: Vec<(i32, i32, i32)> = events
            .iter()
            .filter(|e| is_moving_piston(&e.to))
            .map(|e| e.pos)
            .collect();
        assert_eq!(
            moving_now,
            vec![(8, 5, 4), (7, 5, 4), (6, 5, 4), (5, 5, 4)],
            "every destination cell including the arm must hold a moving_piston on \
             the push tick, far end first; got {events:?}"
        );
        assert_eq!(
            column.block_state(5, 5, 4),
            "minecraft:moving_piston[facing=east,type=normal]",
            "the arm cell holds the animating head, not the head itself, until the commit"
        );
        assert_eq!(
            column.block_state(4, 5, 4),
            "minecraft:piston[extended=true,facing=east]",
            "the base alone flips immediately on an extension"
        );

        // Nothing is due on the push tick or the one after it. The queue is
        // non-empty throughout, so these are absences of *piston* work rather than
        // an empty queue reporting a vacuous pass.
        assert!(!queue.is_empty(), "the push must have scheduled something at all");
        let mut per_tick: Vec<(u64, usize)> = Vec::new();
        for tick in PUSH..=PUSH + 3 {
            let due = queue.drain_due(tick, 256);
            let commits = due.iter().filter(|t| is_finish_kind(&t.kind)).count();
            per_tick.push((tick, commits));
            if tick == PUSH + PISTON_MOVE_DELAY {
                // …and each due entry really does carry a parseable record, which is
                // what `crate::tick`'s commit arm reads.
                let states: Vec<String> = due
                    .iter()
                    .filter(|t| is_finish_kind(&t.kind))
                    .map(|t| parse_finish_kind(&t.kind).expect("a parseable record").moved_state)
                    .collect();
                assert!(
                    states.contains(&"minecraft:gravel".to_string())
                        && states.contains(&"minecraft:dirt".to_string())
                        && states.contains(&"minecraft:stone".to_string())
                        && states.contains(
                            &"minecraft:piston_head[facing=east,short=false,type=normal]"
                                .to_string()
                        ),
                    "the commits must carry the four travelling states; got {states:?}"
                );
            }
        }
        assert_eq!(
            per_tick,
            vec![(PUSH, 0), (PUSH + 1, 0), (PUSH + 2, 4), (PUSH + 3, 0)],
            "four cells must commit on PUSH + 2 and on no other tick"
        );
    }

    /// The **placement/hand-use path** carries the commits too, with the delay
    /// intact — the path a player's own right-click reaches.
    ///
    /// The trigger is a redstone torch, not a lever, and that is a finding rather
    /// than a convenience: `redstone::is_signal_source` is
    /// `torch || diode || observer`, and `weak_signal`/`direct_signal` have no arm
    /// for a `powered=true` lever, button or pressure plate at all. So those three
    /// emit **no signal** in this crate and cannot drive a piston — a pre-existing
    /// gap in the redstone model, not in the move. A gate written around a lever
    /// here reads as "the piston is broken".
    ///
    /// `crate::server::propagate_placement` runs `react_at_placement` at
    /// `current_tick = 0` and hands the drained batch to the tick loop to *rebase*,
    /// so for that hand-over to preserve two ticks the `trigger_tick` in the batch
    /// must be the relative delay itself. Asserting the absolute due tick from the
    /// other gate would pass here for the wrong reason.
    ///
    /// This is also the gate that stops `crate::server::moving_piston_records` from
    /// being an island: it filters exactly this batch, so an empty batch would make
    /// it return nothing forever with nothing red.
    #[test]
    fn the_placement_path_carries_the_commits_with_the_delay_intact() {
        use crate::chunk::ChunkColumn;
        use crate::scheduled_tick::ScheduledTickQueue;

        let mut column = ChunkColumn::new(0, 16);
        column.set_block(4, 5, 4, "minecraft:piston[extended=false,facing=east]");
        column.set_block(5, 5, 4, "minecraft:stone");
        column.set_block(6, 5, 4, "minecraft:dirt");
        column.set_block(4, 5, 5, "minecraft:redstone_torch[lit=true]");

        let mut queue: ScheduledTickQueue<String> = ScheduledTickQueue::new();
        // Exactly `propagate_placement`'s own two calls, in its own order.
        let _ = crate::random_tick::react_at_placement(&mut column, 0, 0, 4, 5, 5, &mut queue, 0);
        let batch = queue.drain_due(u64::MAX, usize::MAX);

        let commits: Vec<(&(i32, i32, i32), u64, String)> = batch
            .iter()
            .filter(|pending| is_finish_kind(&pending.kind))
            .map(|pending| {
                (
                    &pending.pos,
                    pending.trigger_tick,
                    parse_finish_kind(&pending.kind)
                        .expect("a parseable record")
                        .moved_state,
                )
            })
            .collect();

        // Three cells: destinations (6,5,4) and (7,5,4) plus the arm (5,5,4).
        assert_eq!(
            commits.len(),
            3,
            "the placement path must schedule one commit per moving cell; got {batch:?}"
        );
        let mut wrong_delay: Vec<(&(i32, i32, i32), u64)> = Vec::new();
        for (pos, trigger_tick, _) in &commits {
            if *trigger_tick != PISTON_MOVE_DELAY {
                wrong_delay.push((pos, *trigger_tick));
            }
        }
        assert!(
            wrong_delay.is_empty(),
            "every commit's trigger_tick must be the relative delay {PISTON_MOVE_DELAY}, \
             because the tick loop rebases it; wrong: {wrong_delay:?}"
        );
        let mut states: Vec<&str> = commits.iter().map(|(_, _, s)| s.as_str()).collect();
        states.sort_unstable();
        assert_eq!(
            states,
            vec![
                "minecraft:dirt",
                "minecraft:piston_head[facing=east,short=false,type=normal]",
                "minecraft:stone",
            ]
        );
        // And the cells really are holding `moving_piston` right now, so the records
        // above have a state to attach to.
        for x in [5, 6, 7] {
            assert!(
                is_moving_piston(column.block_state(x, 5, 4)),
                "cell ({x},5,4) holds {} rather than a moving_piston",
                column.block_state(x, 5, 4)
            );
        }
    }

    /// [`finish_kind`] round-trips, including a moved state carrying every
    /// punctuation a canonical state string can hold.
    ///
    /// The four fields are given **pairwise-distinguishable** values on purpose:
    /// `extending` and `source` differ, so a transposition of the two adjacent
    /// booleans cannot survive, and the direction is one whose name is not a prefix
    /// of any other.
    #[test]
    fn a_finish_kind_round_trips_and_a_foreign_kind_does_not_parse() {
        let entity = MovingBlockEntity {
            moved_state: "minecraft:piston_head[facing=west,short=true,type=sticky]".to_string(),
            direction: Direction::North,
            extending: false,
            source: true,
        };
        let kind = finish_kind(&entity);
        assert!(is_finish_kind(&kind), "{kind} must be recognised as a commit");
        assert_eq!(parse_finish_kind(&kind).as_ref(), Some(&entity));

        // Controls: the parser must decline everything it did not write, or
        // `crate::tick`'s commit arm would try to write a state out of a repeater's
        // scheduled tick.
        assert_eq!(parse_finish_kind(redstone::TICK_REPEATER), None);
        assert_eq!(parse_finish_kind(TICK_PISTON), None);
        assert!(!is_finish_kind(redstone::TICK_REPEATER));
        assert_eq!(parse_finish_kind(TICK_PISTON_FINISH), None, "the prefix alone is not a record");
        assert_eq!(
            parse_finish_kind("redstone:piston_finish|north|false|true|"),
            None,
            "an empty moved state is not a record"
        );
        assert_eq!(
            parse_finish_kind("redstone:piston_finish|nowhere|false|true|minecraft:stone"),
            None,
            "an unknown direction is not a record"
        );

        // `facing_name`/`direction_named` must be a real bijection over all six —
        // a table missing one arm would make that direction's pistons unparseable
        // and silently strand them mid-animation.
        for direction in ALL_DIRECTIONS {
            assert_eq!(direction_named(facing_name(direction)), Some(direction));
        }
    }

    /// `source` is set for the piston's **own** cell and nothing else, and which
    /// cell that is differs between an extension and a retraction. Getting it wrong
    /// makes a client draw an ordinary block where a head belongs.
    #[test]
    fn only_the_pistons_own_cell_is_a_source() {
        let extend = fake(&[
            (at(0, 0, 0), "minecraft:sticky_piston[extended=false,facing=east]"),
            (at(1, 0, 0), "minecraft:stone"),
            (at(2, 0, 0), "minecraft:dirt"),
        ]);
        let (piston_state, writes) =
            plan_move(&extend, at(0, 0, 0), Direction::East, true).expect("resolves");
        let start = begin_move(&writes, &piston_state, at(0, 0, 0), Direction::East, true);
        let sources: Vec<BlockPos> = start
            .moving
            .iter()
            .filter(|(_, _, e)| e.source)
            .map(|(pos, _, _)| *pos)
            .collect();
        assert_eq!(
            sources,
            vec![at(1, 0, 0)],
            "on an extension exactly the arm cell is the source"
        );
        let (_, moving_state, arm) = start
            .moving
            .iter()
            .find(|(pos, _, _)| *pos == at(1, 0, 0))
            .expect("the arm cell moves");
        assert_eq!(
            arm.moved_state,
            "minecraft:piston_head[facing=east,short=false,type=sticky]"
        );
        assert!(arm.extending);
        assert_eq!(
            moving_state, "minecraft:moving_piston[facing=east,type=sticky]",
            "`moveBlocks` sets TYPE from the piston only for the cell it creates itself"
        );
        let carried = start
            .moving
            .iter()
            .find(|(pos, _, _)| *pos == at(2, 0, 0))
            .expect("the run's first block moves");
        assert_eq!(
            carried.1, "minecraft:moving_piston[facing=east,type=normal]",
            "a carried block gets the default TYPE, not the piston's"
        );
        assert_eq!(
            start.base_now.as_deref(),
            Some("minecraft:sticky_piston[extended=true,facing=east]"),
            "an extension flips the base immediately"
        );

        let retract = fake(&[
            (at(0, 0, 0), "minecraft:sticky_piston[extended=true,facing=east]"),
            (at(1, 0, 0), "minecraft:piston_head[facing=east,short=false,type=sticky]"),
            (at(2, 0, 0), SLIME_BLOCK),
        ]);
        let (piston_state, writes) =
            plan_move(&retract, at(0, 0, 0), Direction::East, false).expect("resolves");
        let start = begin_move(&writes, &piston_state, at(0, 0, 0), Direction::East, false);
        assert_eq!(
            start.base_now, None,
            "a retraction does not write the base now — the base is what animates"
        );
        let (pos, moving_state, base) = start
            .moving
            .iter()
            .find(|(_, _, e)| e.source)
            .expect("a retraction has a source cell");
        assert_eq!(*pos, at(0, 0, 0), "and it is the base cell, not the arm");
        assert_eq!(moving_state, "minecraft:moving_piston[facing=east,type=sticky]");
        assert!(!base.extending);
        assert_eq!(
            base.moved_state, "minecraft:sticky_piston[extended=false,facing=east]",
            "the base's own retracted state is what commits, and it is the record \
             `PistonHeadRenderer` builds a homecoming head from"
        );
    }

    /// The seed a client is given is `0.0`, and that is the value at which the two
    /// readings of `getExtendedProgress` are **furthest apart**: `p - 1` is `-1.0`
    /// and `1 - p` is `+1.0`, a whole cell in opposite directions. At `p = 0.5` they
    /// agree in magnitude, which is why nothing here is gated at 0.5.
    ///
    /// So `extending` is the only thing that tells a client which way a block is
    /// travelling, and it must survive into the record for both directions.
    #[test]
    fn the_progress_seed_is_zero_and_extending_distinguishes_the_two_directions() {
        assert_eq!(PISTON_INITIAL_PROGRESS, 0.0);
        assert_eq!(PISTON_PROGRESS_SPEED, 0.5);
        // Two ticks of ramp at 0.5 lands exactly on 1.0, which is what makes the
        // third tick's `progressO >= 1.0` commit branch fire.
        assert_eq!(
            PISTON_INITIAL_PROGRESS + PISTON_PROGRESS_SPEED * PISTON_MOVE_DELAY as f32,
            1.0
        );
        for (extending, expected_offset) in [(true, -1.0_f32), (false, 1.0_f32)] {
            let signed = if extending {
                PISTON_INITIAL_PROGRESS - 1.0
            } else {
                1.0 - PISTON_INITIAL_PROGRESS
            };
            assert_eq!(
                signed, expected_offset,
                "at the seed the two directions differ by a whole cell"
            );
        }
    }

    /// `Direction.get3DDataValue()` for all six, against vanilla's own enum
    /// declaration order. Pairwise distinct, and every value differs from what an
    /// alphabetical or horizontal-facing ordering would produce for at least one
    /// direction — which is the ordering a hand-count reaches for.
    #[test]
    fn facing_3d_values_follow_vanillas_declaration_order() {
        let of = |direction| MovingBlockEntity {
            moved_state: "minecraft:stone".to_string(),
            direction,
            extending: true,
            source: false,
        }
        .facing_3d_value();
        assert_eq!(of(Direction::Down), 0);
        assert_eq!(of(Direction::Up), 1);
        assert_eq!(of(Direction::North), 2);
        assert_eq!(of(Direction::South), 3);
        assert_eq!(of(Direction::West), 4);
        assert_eq!(of(Direction::East), 5);
        // The alphabetical ordering a hand-count produces is
        // down/east/north/south/up/west, which puts EAST at 1 rather than 5.
        assert_ne!(of(Direction::East), 1, "this is not the alphabetical ordering");
    }
}
