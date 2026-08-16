//! Vanilla's A* land pathfinder: `PathFinder`, `WalkNodeEvaluator`, and `Path`.
//!
//! This combines vanilla's `PathFinder` (the A* loop over a
//! [`BinaryHeap`](super::heap::BinaryHeap) open set) and its
//! `WalkNodeEvaluator` (neighbour generation, step-up/drop-down/water logic,
//! per-mob path-type aggregation). The two are separate classes in vanilla but
//! share a mutable node pool; in Rust a single owning [`Search`] over a node
//! arena expresses that sharing without interior mutability, while the method
//! names keep the correspondence obvious.
//!
//! What is reproduced faithfully, because it changes where a real mob goes:
//! the `1.5` heuristic fudge, the `g + distance + costMalus` edge cost, the
//! per-mob bounding-box aggregation of path types, the jump-up recursion, the
//! drop-down fall-distance limit, and the malus table from [`PathType`].

use super::heap::BinaryHeap;
use super::node::{NO_PARENT, Node, PathType};
use super::world::{Aabb, MobShape, PathWorld};
use lodestone_model::BlockPos;
use std::collections::HashMap;

/// Vanilla's heuristic fudge factor applied to a neighbour's `h`.
const FUDGING: f32 = 1.5;
/// Vanilla's default mob jump height floor.
const DEFAULT_MOB_JUMP_HEIGHT: f64 = 1.125;

/// A description of where the mob is starting from.
#[derive(Debug, Clone, Copy)]
pub struct PathStart {
    /// Mob feet X (world).
    pub x: f64,
    /// Mob feet Y (world).
    pub y: f64,
    /// Mob feet Z (world).
    pub z: f64,
    /// Whether the mob is on the ground.
    pub on_ground: bool,
    /// Whether the mob is in water.
    pub in_water: bool,
}

impl PathStart {
    /// A convenience constructor for a grounded mob.
    #[must_use]
    pub fn grounded(x: f64, y: f64, z: f64) -> Self {
        Self {
            x,
            y,
            z,
            on_ground: true,
            in_water: false,
        }
    }
}

/// One waypoint of a computed path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathNode {
    /// Block X.
    pub x: i32,
    /// Block Y.
    pub y: i32,
    /// Block Z.
    pub z: i32,
    /// The classified type at this waypoint.
    pub kind: PathType,
}

impl PathNode {
    /// This waypoint as a block position.
    #[must_use]
    pub fn block_pos(&self) -> BlockPos {
        BlockPos::new(self.x, self.y, self.z)
    }
}

/// A computed path: an ordered list of waypoints plus follow state.
#[derive(Debug, Clone)]
pub struct Path {
    nodes: Vec<PathNode>,
    target: BlockPos,
    dist_to_target: f32,
    reached: bool,
    next_index: usize,
}

impl Path {
    /// Creates a path from waypoints toward `target`.
    #[must_use]
    pub fn new(nodes: Vec<PathNode>, target: BlockPos, reached: bool) -> Self {
        let dist_to_target = nodes.last().map_or(f32::MAX, |n| {
            let xd = (target.x - n.x).abs() as f32;
            let yd = (target.y - n.y).abs() as f32;
            let zd = (target.z - n.z).abs() as f32;
            xd + yd + zd
        });
        Self {
            nodes,
            target,
            dist_to_target,
            reached,
            next_index: 0,
        }
    }

    /// The waypoints, in order.
    #[must_use]
    pub fn nodes(&self) -> &[PathNode] {
        &self.nodes
    }

    /// The number of waypoints.
    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Whether the path has no waypoints.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Whether the search actually reached a target block (vs. a best-effort
    /// closest approach).
    #[must_use]
    pub fn reached(&self) -> bool {
        self.reached
    }

    /// The Manhattan distance from the last waypoint to the requested target.
    #[must_use]
    pub fn dist_to_target(&self) -> f32 {
        self.dist_to_target
    }

    /// The requested target block.
    #[must_use]
    pub fn target(&self) -> BlockPos {
        self.target
    }

    /// The next waypoint to walk toward, if any remain.
    #[must_use]
    pub fn next_node(&self) -> Option<PathNode> {
        self.nodes.get(self.next_index).copied()
    }

    /// A waypoint by index.
    #[must_use]
    pub fn node(&self, i: usize) -> Option<PathNode> {
        self.nodes.get(i).copied()
    }

    /// The final waypoint.
    #[must_use]
    pub fn end_node(&self) -> Option<PathNode> {
        self.nodes.last().copied()
    }

    /// Advances to the next waypoint.
    pub fn advance(&mut self) {
        self.next_index += 1;
    }

    /// Whether every waypoint has been consumed.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.next_index >= self.nodes.len()
    }
}

/// Tuning knobs for a single [`PathFinder::find_path`] call.
#[derive(Debug, Clone, Copy)]
pub struct PathParams {
    /// Bounds how far (in walked distance) the search explores.
    pub max_path_length: f32,
    /// Manhattan distance at which a target counts as reached.
    pub reach_range: i32,
    /// Scales the visited-node budget for this search.
    pub visited_multiplier: f32,
}

impl Default for PathParams {
    fn default() -> Self {
        Self {
            max_path_length: 0.0,
            reach_range: 1,
            visited_multiplier: 1.0,
        }
    }
}

/// A land pathfinder. Construct once per mob (its `max_visited_nodes` derives
/// from the mob's follow range) and reuse across searches.
#[derive(Debug, Clone)]
pub struct PathFinder {
    /// The visited-node budget before the search gives up.
    pub max_visited_nodes: i32,
}

impl PathFinder {
    /// Creates a pathfinder with the given visited-node budget. Vanilla derives
    /// this as `floor(followRange * 16)`.
    #[must_use]
    pub fn new(max_visited_nodes: i32) -> Self {
        Self { max_visited_nodes }
    }

    /// Finds a path from `start` to the nearest of `targets`, using the search
    /// tuning in `params`.
    ///
    /// Returns `None` if no start node is valid or no route (even a partial one)
    /// exists.
    #[must_use]
    pub fn find_path(
        &self,
        world: &dyn PathWorld,
        mob: &MobShape,
        start: PathStart,
        targets: &[BlockPos],
        params: PathParams,
    ) -> Option<Path> {
        let PathParams {
            max_path_length,
            reach_range,
            visited_multiplier,
        } = params;
        if targets.is_empty() {
            return None;
        }
        let mut search = Search::new(world, mob);
        let from = search.start_node(start)?;
        let target_infos: Vec<TargetInfo> = targets
            .iter()
            .map(|&pos| {
                let node = BlockPos::new(pos.x, pos.y, pos.z);
                TargetInfo {
                    x: node.x,
                    y: node.y,
                    z: node.z,
                    block_pos: pos,
                    best_h: f32::MAX,
                    best_node: NO_PARENT,
                    reached: false,
                }
            })
            .collect();
        let budget = (self.max_visited_nodes as f32 * visited_multiplier) as i32;
        search.run(from, target_infos, max_path_length, reach_range, budget)
    }
}

struct TargetInfo {
    x: i32,
    y: i32,
    z: i32,
    block_pos: BlockPos,
    best_h: f32,
    best_node: usize,
    reached: bool,
}

/// The owning search state: node arena, dedup map, open-set heap, plus caches.
struct Search<'a> {
    world: &'a dyn PathWorld,
    mob: &'a MobShape,
    arena: Vec<Node>,
    nodes: HashMap<i32, usize>,
    heap: BinaryHeap,
    type_cache: HashMap<i64, PathType>,
    mob_block: BlockPos,
    mob_x: f64,
    mob_y: f64,
    mob_z: f64,
}

impl<'a> Search<'a> {
    fn new(world: &'a dyn PathWorld, mob: &'a MobShape) -> Self {
        Self {
            world,
            mob,
            arena: Vec::new(),
            nodes: HashMap::new(),
            heap: BinaryHeap::new(),
            type_cache: HashMap::new(),
            mob_block: BlockPos::new(0, 0, 0),
            mob_x: 0.0,
            mob_y: 0.0,
            mob_z: 0.0,
        }
    }

    fn get_node(&mut self, x: i32, y: i32, z: i32) -> usize {
        let hash = Node::hash(x, y, z);
        if let Some(&idx) = self.nodes.get(&hash) {
            return idx;
        }
        let idx = self.arena.len();
        self.arena.push(Node::new(x, y, z));
        self.nodes.insert(hash, idx);
        idx
    }

    fn coords(&self, idx: usize) -> (i32, i32, i32) {
        let n = &self.arena[idx];
        (n.x, n.y, n.z)
    }

    fn mob_jump_height(&self) -> f64 {
        DEFAULT_MOB_JUMP_HEIGHT.max(f64::from(self.mob.max_up_step))
    }

    fn floor_level(&self, x: i32, y: i32, z: i32) -> f64 {
        if self.mob.can_float && self.world.is_water(x, y, z) {
            return f64::from(y) + 0.5;
        }
        // getFloorLevel(level, pos): use the block below.
        f64::from(y - 1) + self.world.collision_top(x, y - 1, z)
    }

    // ---- version-free path-type classification (getPathTypeStatic etc.) ----

    fn path_type_static(&self, x: i32, y: i32, z: i32) -> PathType {
        let bt = self.world.base_path_type(x, y, z);
        if bt == PathType::Open && y > self.world.min_y() {
            match self.world.base_path_type(x, y - 1, z) {
                PathType::Open | PathType::Water | PathType::Lava | PathType::Walkable => {
                    PathType::Open
                }
                PathType::Fire => PathType::Fire,
                PathType::Damaging => PathType::Damaging,
                PathType::StickyHoney => PathType::StickyHoney,
                PathType::PowderSnow => PathType::OnTopOfPowderSnow,
                PathType::DamageCautious => PathType::DamageCautious,
                PathType::Trapdoor => PathType::OnTopOfTrapdoor,
                _ => self.check_neighbour_blocks(x, y, z),
            }
        } else {
            bt
        }
    }

    fn check_neighbour_blocks(&self, x: i32, y: i32, z: i32) -> PathType {
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    if dx == 0 && dz == 0 {
                        continue;
                    }
                    match self.world.base_path_type(x + dx, y + dy, z + dz) {
                        PathType::Damaging => return PathType::DamagingInNeighbor,
                        PathType::Fire | PathType::Lava => return PathType::FireInNeighbor,
                        PathType::Water => return PathType::WaterBorder,
                        PathType::DamageCautious => return PathType::DamageCautious,
                        _ => {}
                    }
                }
            }
        }
        PathType::Walkable
    }

    fn path_type_within_mob_bb(&self, x: i32, y: i32, z: i32) -> Vec<PathType> {
        let mut set: Vec<PathType> = Vec::new();
        let mob_rail = self.path_type_static(self.mob_block.x, self.mob_block.y, self.mob_block.z);
        let mob_rail_below =
            self.path_type_static(self.mob_block.x, self.mob_block.y - 1, self.mob_block.z);
        for dx in 0..self.mob.cell_width() {
            for dy in 0..self.mob.cell_height() {
                for dz in 0..self.mob.cell_width() {
                    let mut bt = self.path_type_static(x + dx, y + dy, z + dz);
                    if bt == PathType::DoorWoodClosed
                        && self.mob.can_open_doors
                        && self.mob.can_pass_doors
                    {
                        bt = PathType::WalkableDoor;
                    }
                    if bt == PathType::DoorOpen && !self.mob.can_pass_doors {
                        bt = PathType::Blocked;
                    }
                    if bt == PathType::Rail
                        && mob_rail != PathType::Rail
                        && mob_rail_below != PathType::Rail
                    {
                        bt = PathType::UnpassableRail;
                    }
                    if !set.contains(&bt) {
                        set.push(bt);
                    }
                }
            }
        }
        set
    }

    fn path_type_of_mob(&self, x: i32, y: i32, z: i32) -> PathType {
        let mut set = self.path_type_within_mob_bb(x, y, z);
        if set.len() == 1 {
            return set[0];
        }
        if set.contains(&PathType::Fence) {
            return PathType::Fence;
        }
        if set.contains(&PathType::UnpassableRail) {
            return PathType::UnpassableRail;
        }
        // Iterate in enum-declaration order to match vanilla's EnumSet ordering,
        // which decides the `>=` tie-break.
        set.sort_by_key(|pt| ordinal(*pt));
        let mut highest_type = PathType::Blocked;
        let mut highest_malus = self.mob.malus(PathType::Blocked);
        for &pt in &set {
            let m = self.mob.malus(pt);
            if m < 0.0 {
                return pt;
            }
            if m >= highest_malus {
                highest_malus = m;
                highest_type = pt;
            }
        }
        let current = self.path_type_static(x, y, z);
        if self.mob.cell_width() > 1 {
            let current_cheaper = self.mob.malus(current) < highest_malus;
            let cap =
                current_cheaper && self.mob.malus(PathType::BigMobsCloseToDanger) < highest_malus;
            if cap {
                PathType::BigMobsCloseToDanger
            } else {
                highest_type
            }
        } else if current == PathType::Open
            && highest_type != PathType::Open
            && highest_malus == 0.0
        {
            PathType::Open
        } else {
            highest_type
        }
    }

    fn cached_path_type(&mut self, x: i32, y: i32, z: i32) -> PathType {
        let key = pack(x, y, z);
        if let Some(&t) = self.type_cache.get(&key) {
            return t;
        }
        let t = self.path_type_of_mob(x, y, z);
        self.type_cache.insert(key, t);
        t
    }

    // ---- start node ----

    fn start_node(&mut self, start: PathStart) -> Option<usize> {
        self.mob_x = start.x;
        self.mob_y = start.y;
        self.mob_z = start.z;
        let block_x = start.x.floor() as i32;
        let block_z = start.z.floor() as i32;
        let mut start_y = start.y.floor() as i32;

        if self.mob.can_float && start.in_water {
            while self.world.is_water(block_x, start_y, block_z) {
                start_y += 1;
            }
            start_y -= 1;
        } else if start.on_ground {
            start_y = (start.y + 0.5).floor() as i32;
        } else {
            // Fall to the first non-air, non-pathfindable block below.
            let mut y = (start.y + 1.0).floor() as i32;
            while y > self.world.min_y() {
                start_y = y;
                y -= 1;
                let below = self.world.base_path_type(block_x, y, block_z);
                if below != PathType::Open && below != PathType::Blocked {
                    break;
                }
                if below == PathType::Blocked {
                    break;
                }
            }
        }

        self.mob_block = BlockPos::new(block_x, start_y, block_z);
        let idx = self.get_node(block_x, start_y, block_z);
        let kind = self.cached_path_type(block_x, start_y, block_z);
        self.arena[idx].kind = kind;
        self.arena[idx].cost_malus = self.mob.malus(kind);
        Some(idx)
    }

    // ---- neighbour generation ----

    fn does_block_have_partial_collision(kind: PathType) -> bool {
        matches!(
            kind,
            PathType::Fence | PathType::DoorWoodClosed | PathType::DoorIronClosed
        )
    }

    fn node_and_update_cost_to_max(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        kind: PathType,
        cost: f32,
    ) -> usize {
        let idx = self.get_node(x, y, z);
        self.arena[idx].kind = kind;
        self.arena[idx].cost_malus = self.arena[idx].cost_malus.max(cost);
        idx
    }

    fn get_blocked_node(&mut self, x: i32, y: i32, z: i32) -> usize {
        let idx = self.get_node(x, y, z);
        self.arena[idx].kind = PathType::Blocked;
        self.arena[idx].cost_malus = -1.0;
        idx
    }

    fn get_closed_node(&mut self, x: i32, y: i32, z: i32, kind: PathType) -> usize {
        let idx = self.get_node(x, y, z);
        self.arena[idx].closed = true;
        self.arena[idx].kind = kind;
        self.arena[idx].cost_malus = kind.malus();
        idx
    }

    #[allow(clippy::too_many_arguments)]
    fn find_accepted_node(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        jump_size: i32,
        node_height: f64,
        travel: (i32, i32),
        current_type: PathType,
    ) -> Option<usize> {
        let max_y_target = self.floor_level(x, y, z);
        if max_y_target - node_height > self.mob_jump_height() {
            return None;
        }
        let path_type = self.cached_path_type(x, y, z);
        let path_cost = self.mob.malus(path_type);
        let mut best: Option<usize> = None;
        if path_cost >= 0.0 {
            best = Some(self.node_and_update_cost_to_max(x, y, z, path_type, path_cost));
        }

        if Self::does_block_have_partial_collision(current_type)
            && let Some(b) = best
            && self.arena[b].cost_malus >= 0.0
            && !self.can_reach_without_collision(b)
        {
            best = None;
        }

        if path_type != PathType::Walkable {
            let best_negative = best.is_none_or(|b| self.arena[b].cost_malus < 0.0);
            if best_negative
                && jump_size > 0
                && (path_type != PathType::Fence || self.mob.can_walk_over_fences)
                && path_type != PathType::UnpassableRail
                && path_type != PathType::Trapdoor
                && path_type != PathType::PowderSnow
            {
                best = self.try_jump_on(x, y, z, jump_size, node_height, travel, current_type);
            } else if path_type == PathType::Water && !self.mob.can_float {
                best = self.try_find_first_non_water_below(x, y, z, best);
            } else if path_type == PathType::Open {
                best = Some(self.try_find_first_ground_node_below(x, y, z));
            } else if Self::does_block_have_partial_collision(path_type) && best.is_none() {
                best = Some(self.get_closed_node(x, y, z, path_type));
            }
        }
        best
    }

    #[allow(clippy::too_many_arguments)]
    fn try_jump_on(
        &mut self,
        x: i32,
        y: i32,
        z: i32,
        jump_size: i32,
        node_height: f64,
        travel: (i32, i32),
        current_type: PathType,
    ) -> Option<usize> {
        let above = self.find_accepted_node(
            x,
            y + 1,
            z,
            jump_size - 1,
            node_height,
            travel,
            current_type,
        )?;
        let (ax, ay, az) = self.coords(above);
        let above_kind = self.arena[above].kind;
        if self.mob.width >= 1.0 {
            return Some(above);
        }
        if above_kind != PathType::Open && above_kind != PathType::Walkable {
            return Some(above);
        }
        // Ensure the mob's box clears the step-up gap.
        let center_x = f64::from(x - travel.0) + 0.5;
        let center_z = f64::from(z - travel.1) + 0.5;
        let half = f64::from(self.mob.width) / 2.0;
        let floor_here = self.floor_level(center_x.floor() as i32, y + 1, center_z.floor() as i32);
        let floor_above = self.floor_level(ax, ay, az);
        let grow = Aabb::new(
            center_x - half,
            floor_here + 0.001,
            center_z - half,
            center_x + half,
            f64::from(self.mob.height) + floor_above - 0.002,
            center_z + half,
        );
        if self.world.collides(grow) {
            None
        } else {
            Some(above)
        }
    }

    fn try_find_first_non_water_below(
        &mut self,
        x: i32,
        mut y: i32,
        z: i32,
        mut best: Option<usize>,
    ) -> Option<usize> {
        y -= 1;
        while y > self.world.min_y() {
            let kind = self.cached_path_type(x, y, z);
            if kind != PathType::Water {
                return best;
            }
            best = Some(self.node_and_update_cost_to_max(x, y, z, kind, self.mob.malus(kind)));
            y -= 1;
        }
        best
    }

    fn try_find_first_ground_node_below(&mut self, x: i32, y: i32, z: i32) -> usize {
        let mut current_y = y - 1;
        while current_y >= self.world.min_y() {
            if y - current_y > self.mob.max_fall_distance {
                return self.get_blocked_node(x, current_y, z);
            }
            let kind = self.cached_path_type(x, current_y, z);
            let cost = self.mob.malus(kind);
            if kind != PathType::Open {
                if cost >= 0.0 {
                    return self.node_and_update_cost_to_max(x, current_y, z, kind, cost);
                }
                return self.get_blocked_node(x, current_y, z);
            }
            current_y -= 1;
        }
        self.get_blocked_node(x, y, z)
    }

    fn can_reach_without_collision(&self, to: usize) -> bool {
        let (tx, ty, tz) = self.coords(to);
        let mut bb = Aabb::new(
            self.mob_x - f64::from(self.mob.width) / 2.0,
            self.mob_y,
            self.mob_z - f64::from(self.mob.width) / 2.0,
            self.mob_x + f64::from(self.mob.width) / 2.0,
            self.mob_y + f64::from(self.mob.height),
            self.mob_z + f64::from(self.mob.width) / 2.0,
        );
        let dx = f64::from(tx) - self.mob_x + bb.x_size() / 2.0;
        let dy = f64::from(ty) - self.mob_y + bb.y_size() / 2.0;
        let dz = f64::from(tz) - self.mob_z + bb.z_size() / 2.0;
        let len = (dx * dx + dy * dy + dz * dz).sqrt();
        let steps = (len / bb.size()).ceil() as i32;
        if steps <= 0 {
            return true;
        }
        let sx = dx / f64::from(steps);
        let sy = dy / f64::from(steps);
        let sz = dz / f64::from(steps);
        for _ in 1..=steps {
            bb = bb.moved(sx, sy, sz);
            if self.world.collides(bb) {
                return false;
            }
        }
        true
    }

    /// Returns up to 8 neighbour arena indices for `pos`.
    fn get_neighbors(&mut self, pos: usize) -> Vec<usize> {
        let (px, py, pz) = self.coords(pos);
        let mut result = Vec::with_capacity(8);
        let above = self.cached_path_type(px, py + 1, pz);
        let current = self.cached_path_type(px, py, pz);
        let jump_size = if self.mob.malus(above) >= 0.0 && current != PathType::StickyHoney {
            (1.0_f32).max(self.mob.max_up_step).floor() as i32
        } else {
            0
        };
        let pos_height = self.floor_level(px, py, pz);

        // Direction.Plane.HORIZONTAL order: SOUTH, WEST, NORTH, EAST.
        let dirs = [(0, 1), (-1, 0), (0, -1), (1, 0)];
        let mut cardinal: [Option<usize>; 4] = [None; 4];
        for (i, &(sx, sz)) in dirs.iter().enumerate() {
            let node = self.find_accepted_node(
                px + sx,
                py,
                pz + sz,
                jump_size,
                pos_height,
                (sx, sz),
                current,
            );
            cardinal[i] = node;
            if let Some(n) = node
                && self.is_neighbor_valid(n, pos)
            {
                result.push(n);
            }
        }

        // Diagonals pair each direction with its clockwise successor.
        for i in 0..4 {
            let j = (i + 1) % 4;
            if self.is_diagonal_pair_valid(pos, cardinal[i], cardinal[j]) {
                let (sx, sz) = dirs[i];
                let (tx, tz) = dirs[j];
                let diag = self.find_accepted_node(
                    px + sx + tx,
                    py,
                    pz + sz + tz,
                    jump_size,
                    pos_height,
                    dirs[i],
                    current,
                );
                if self.is_diagonal_valid(diag)
                    && let Some(d) = diag
                {
                    result.push(d);
                }
            }
        }
        result
    }

    fn is_neighbor_valid(&self, neighbor: usize, current: usize) -> bool {
        !self.arena[neighbor].closed
            && (self.arena[neighbor].cost_malus >= 0.0 || self.arena[current].cost_malus < 0.0)
    }

    fn is_diagonal_pair_valid(&self, pos: usize, ew: Option<usize>, ns: Option<usize>) -> bool {
        let (Some(ew), Some(ns)) = (ew, ns) else {
            return false;
        };
        let py = self.arena[pos].y;
        if self.arena[ns].y > py || self.arena[ew].y > py {
            return false;
        }
        if self.arena[ew].kind == PathType::WalkableDoor
            || self.arena[ns].kind == PathType::WalkableDoor
        {
            return false;
        }
        let big = self.mob.width > 1.0;
        if big && (self.arena[ew].cost_malus > 0.0 || self.arena[ns].cost_malus > 0.0) {
            return false;
        }
        let can_pass_posts = self.arena[ns].kind == PathType::Fence
            && self.arena[ew].kind == PathType::Fence
            && self.mob.width < 0.5;
        (self.arena[ns].y < py || self.arena[ns].cost_malus >= 0.0 || can_pass_posts)
            && (self.arena[ew].y < py || self.arena[ew].cost_malus >= 0.0 || can_pass_posts)
    }

    fn is_diagonal_valid(&self, diagonal: Option<usize>) -> bool {
        match diagonal {
            None => false,
            Some(d) => {
                !self.arena[d].closed
                    && self.arena[d].kind != PathType::WalkableDoor
                    && self.arena[d].cost_malus >= 0.0
            }
        }
    }

    // ---- A* loop ----

    fn best_h(&mut self, node: usize, targets: &mut [TargetInfo]) -> f32 {
        let (nx, ny, nz) = self.coords(node);
        let mut best = f32::MAX;
        for t in targets.iter_mut() {
            let xd = (t.x - nx) as f32;
            let yd = (t.y - ny) as f32;
            let zd = (t.z - nz) as f32;
            let h = (xd * xd + yd * yd + zd * zd).sqrt();
            if h < t.best_h {
                t.best_h = h;
                t.best_node = node;
            }
            best = best.min(h);
        }
        best
    }

    fn run(
        &mut self,
        from: usize,
        mut targets: Vec<TargetInfo>,
        max_path_length: f32,
        reach_range: i32,
        budget: i32,
    ) -> Option<Path> {
        self.arena[from].g = 0.0;
        let h = self.best_h(from, &mut targets);
        self.arena[from].h = h;
        self.arena[from].f = h;
        self.heap.clear();
        {
            let Self { heap, arena, .. } = self;
            heap.insert(arena, from);
        }
        let mut count = 0;
        let mut any_reached = false;

        while !self.heap.is_empty() {
            count += 1;
            if count >= budget {
                break;
            }
            let current = {
                let Self { heap, arena, .. } = self;
                heap.pop(arena)
            };
            self.arena[current].closed = true;

            let (cx, cy, cz) = self.coords(current);
            for t in targets.iter_mut() {
                let manhattan = (t.x - cx).abs() + (t.y - cy).abs() + (t.z - cz).abs();
                if manhattan <= reach_range {
                    t.reached = true;
                    any_reached = true;
                }
            }
            if any_reached {
                break;
            }

            if self.arena[current].distance_to(&self.arena[from]) >= max_path_length {
                continue;
            }

            let neighbors = self.get_neighbors(current);
            for neighbor in neighbors {
                let distance = self.arena[current].distance_to(&self.arena[neighbor]);
                let walked = self.arena[current].walked_distance + distance;
                self.arena[neighbor].walked_distance = walked;
                let tentative_g =
                    self.arena[current].g + distance + self.arena[neighbor].cost_malus;
                let in_open = self.arena[neighbor].in_open_set();
                if walked < max_path_length && (!in_open || tentative_g < self.arena[neighbor].g) {
                    self.arena[neighbor].came_from = current;
                    self.arena[neighbor].g = tentative_g;
                    let hh = self.best_h(neighbor, &mut targets) * FUDGING;
                    self.arena[neighbor].h = hh;
                    let f = self.arena[neighbor].g + hh;
                    if in_open {
                        let Self { heap, arena, .. } = self;
                        heap.change_cost(arena, neighbor, f);
                    } else {
                        self.arena[neighbor].f = f;
                        let Self { heap, arena, .. } = self;
                        heap.insert(arena, neighbor);
                    }
                }
            }
        }

        self.reconstruct_best(&targets, any_reached)
    }

    fn reconstruct_best(&self, targets: &[TargetInfo], any_reached: bool) -> Option<Path> {
        let candidates: Vec<&TargetInfo> = if any_reached {
            targets.iter().filter(|t| t.reached).collect()
        } else {
            targets.iter().collect()
        };
        let mut best_path: Option<Path> = None;
        for t in candidates {
            if t.best_node == NO_PARENT {
                continue;
            }
            let path = self.reconstruct(t.best_node, t.block_pos, any_reached);
            best_path = Some(match best_path {
                None => path,
                Some(existing) => {
                    if any_reached {
                        if path.len() < existing.len() {
                            path
                        } else {
                            existing
                        }
                    } else if path.dist_to_target < existing.dist_to_target
                        || (path.dist_to_target == existing.dist_to_target
                            && path.len() < existing.len())
                    {
                        path
                    } else {
                        existing
                    }
                }
            });
        }
        best_path
    }

    fn reconstruct(&self, closest: usize, target: BlockPos, reached: bool) -> Path {
        let mut chain = Vec::new();
        let mut idx = closest;
        loop {
            let n = &self.arena[idx];
            chain.push(PathNode {
                x: n.x,
                y: n.y,
                z: n.z,
                kind: n.kind,
            });
            if n.came_from == NO_PARENT {
                break;
            }
            idx = n.came_from;
        }
        chain.reverse();
        Path::new(chain, target, reached)
    }
}

fn ordinal(pt: PathType) -> u8 {
    match pt {
        PathType::Blocked => 0,
        PathType::Open => 1,
        PathType::Walkable => 2,
        PathType::WalkableDoor => 3,
        PathType::Trapdoor => 4,
        PathType::PowderSnow => 5,
        PathType::OnTopOfPowderSnow => 6,
        PathType::Fence => 7,
        PathType::Lava => 8,
        PathType::Water => 9,
        PathType::WaterBorder => 10,
        PathType::Rail => 11,
        PathType::UnpassableRail => 12,
        PathType::FireInNeighbor => 13,
        PathType::Fire => 14,
        PathType::DamagingInNeighbor => 15,
        PathType::Damaging => 16,
        PathType::DoorOpen => 17,
        PathType::DoorWoodClosed => 18,
        PathType::DoorIronClosed => 19,
        PathType::Breach => 20,
        PathType::Leaves => 21,
        PathType::StickyHoney => 22,
        PathType::Cocoa => 23,
        PathType::DamageCautious => 24,
        PathType::OnTopOfTrapdoor => 25,
        PathType::BigMobsCloseToDanger => 26,
    }
}

fn pack(x: i32, y: i32, z: i32) -> i64 {
    // BlockPos.asLong-style packing, sufficient as a cache key.
    ((x as i64 & 0x3FF_FFFF) << 38) | ((z as i64 & 0x3FF_FFFF) << 12) | (y as i64 & 0xFFF)
}
