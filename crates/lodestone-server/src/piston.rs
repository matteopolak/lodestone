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
//! ## What is deliberately not here, and why #316 stays open
//!
//! **The two-phase `MovingPistonBlock` transition is not modelled.** Vanilla
//! replaces the head cell with a `moving_piston` block entity for the duration of
//! the animation and finishes the move on a later tick; [`apply_move`] applies the
//! final positions in one step. Consequences, all named rather than discovered:
//!
//! * A **0-tick pulse** generator depends on the piston being retracted *during*
//!   its own extension, which needs the intermediate `moving_piston` state to exist
//!   and be interruptible. It cannot work here.
//! * `TRIGGER_DROP` (block event 2, the "the head is mid-extension, drop it"
//!   case) has no distinct behaviour, because there is never a mid-extension.
//! * Entities in the push path are not shoved: that is `PistonMovingBlockEntity`'s
//!   `moveEntities`, which needs the same intermediate state plus an entity AABB
//!   sweep this crate has no piston-aware collision pass for.
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
//! * To model the animation, the missing piece is a `moving_piston` block entity in
//!   [`crate::block_entities`] plus a two-stage scheduled tick, not a change here:
//!   [`resolve`] already answers the question the second stage would ask.
//!
//! ## Dependencies
//!
//! [`crate::redstone`] for the signal query and the state-string helpers, and
//! [`crate::neighbor_update::Direction`]. No block-state census: push reaction is
//! per *block*, not per state, so a name table is the right shape.

use lodestone_model::BlockPos;

use crate::neighbor_update::{ALL_DIRECTIONS, Direction};
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
}
