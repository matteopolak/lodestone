//! The `tree` feature: trunk placer, foliage placer, decorators, then the leaf-distance pass.
//!
//! One placement is a [`Run`]: it owns the four position sets the reference collects (trunk,
//! foliage, roots, decorations; hash sets whose iteration order decorators observe) and
//! writes straight into the level. Placers that are not ported are recorded in
//! [`TreeConfig::unsupported`], which keeps their placed features out of the oracle replay.

use serde_json::Value;

use crate::blocks::{BlockId, Dir, FluidKind, State};
use crate::env::Env;
use crate::javaset::JavaSet;
use crate::json::{Res, array, boolean, get, int, int_or, obj, type_of};
use crate::level::Level;
use crate::pos::{Pos, Rng};
use crate::provider::IntProvider;
use crate::survive::{self, Kind};
use crate::stateprovider::StateProvider;
use crate::tags::BlockSet;

pub mod decorator;
pub mod fallen;
pub mod foliage;
pub mod trunk;

use decorator::Decorator;
use foliage::FoliagePlacer;
use trunk::TrunkPlacer;

/// How wide the space around the trunk must be at each height.
#[derive(Clone, Debug)]
pub enum FeatureSize {
    TwoLayers { limit: i32, lower: i32, upper: i32, min_clipped: Option<i32> },
    ThreeLayers { limit: i32, upper_limit: i32, lower: i32, middle: i32, upper: i32, min_clipped: Option<i32> },
}

impl FeatureSize {
    fn parse(v: &Value, ctx: &str) -> Res<Self> {
        let min_clipped = match v.get("min_clipped_height") {
            Some(n) => Some(n.as_i64().ok_or_else(|| format!("{ctx}: min_clipped_height"))? as i32),
            None => None,
        };
        Ok(match type_of(v, ctx)? {
            "two_layers_feature_size" => Self::TwoLayers {
                limit: int_or(v, "limit", 1, ctx)?,
                lower: int_or(v, "lower_size", 0, ctx)?,
                upper: int_or(v, "upper_size", 1, ctx)?,
                min_clipped,
            },
            "three_layers_feature_size" => Self::ThreeLayers {
                limit: int_or(v, "limit", 1, ctx)?,
                upper_limit: int_or(v, "upper_limit", 1, ctx)?,
                lower: int_or(v, "lower_size", 0, ctx)?,
                middle: int_or(v, "middle_size", 1, ctx)?,
                upper: int_or(v, "upper_size", 1, ctx)?,
                min_clipped,
            },
            other => return Err(format!("{ctx}: unknown feature size `{other}`")),
        })
    }

    fn min_clipped(&self) -> Option<i32> {
        match self {
            Self::TwoLayers { min_clipped, .. } | Self::ThreeLayers { min_clipped, .. } => *min_clipped,
        }
    }

    fn at_height(&self, tree_height: i32, y: i32) -> i32 {
        match self {
            Self::TwoLayers { limit, lower, upper, .. } => {
                if y < *limit {
                    *lower
                } else {
                    *upper
                }
            }
            Self::ThreeLayers { limit, upper_limit, lower, middle, upper, .. } => {
                if y < *limit {
                    *lower
                } else if y >= tree_height - upper_limit {
                    *upper
                } else {
                    *middle
                }
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct TreeConfig {
    pub trunk: StateProvider,
    pub foliage: StateProvider,
    pub below_trunk: StateProvider,
    pub trunk_placer: TrunkPlacer,
    pub foliage_placer: FoliagePlacer,
    pub size: FeatureSize,
    pub decorators: Vec<Decorator>,
    pub ignore_vines: bool,
    /// Placer, decorator and root types that are not ported (the tree places nothing).
    pub unsupported: Vec<String>,
}

impl TreeConfig {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        let mut unsupported = Vec::new();
        if let Some(root) = v.get("root_placer") {
            unsupported.push(format!("root_placer {}", type_of(root, ctx)?));
        }
        let trunk_placer = TrunkPlacer::parse(env, get(v, "trunk_placer", ctx)?, ctx, &mut unsupported)?;
        let foliage_placer = FoliagePlacer::parse(get(v, "foliage_placer", ctx)?, ctx, &mut unsupported)?;
        let mut decorators = Vec::new();
        for d in array(v, "decorators", ctx)? {
            decorators.push(Decorator::parse(env, d, ctx, &mut unsupported)?);
        }
        Ok(Self {
            trunk: StateProvider::parse(env, get(v, "trunk_provider", ctx)?, ctx)?,
            foliage: StateProvider::parse(env, get(v, "foliage_provider", ctx)?, ctx)?,
            below_trunk: StateProvider::parse(env, get(v, "below_trunk_provider", ctx)?, ctx)?,
            trunk_placer,
            foliage_placer,
            size: FeatureSize::parse(get(v, "minimum_size", ctx)?, ctx)?,
            decorators,
            ignore_vines: boolean(v, "ignore_vines", false, ctx)?,
            unsupported,
        })
    }
}

/// An int provider read from either a provider document or a bare number.
pub(super) fn int_provider(v: &Value, key: &str, ctx: &str) -> Res<IntProvider> {
    IntProvider::parse(get(v, key, ctx)?, ctx)
}

/// A uniform range written without a `type` (`{min_inclusive, max_inclusive}`).
pub(super) fn plain_uniform(v: &Value, key: &str, ctx: &str) -> Res<(i32, i32)> {
    let o = get(v, key, ctx)?;
    obj(o, ctx)?;
    Ok((int(o, "min_inclusive", ctx)?, int(o, "max_inclusive", ctx)?))
}

/// One tree placement in progress.
#[allow(missing_debug_implementations)]
pub struct Run<'a, 'l> {
    pub level: &'a mut Level<'l>,
    pub rng: &'a mut Rng,
    pub cfg: &'a TreeConfig,
    pub trunks: JavaSet,
    pub foliage: JavaSet,
    pub roots: JavaSet,
    pub decorations: JavaSet,
    pub env: &'l Env,
    leaves: &'l BlockSet,
    logs: &'l BlockSet,
    replaceable: &'l BlockSet,
    vine: BlockId,
}

impl<'a, 'l> Run<'a, 'l> {
    pub(super) fn new(level: &'a mut Level<'l>, rng: &'a mut Rng, cfg: &'a TreeConfig) -> Self {
        let env = level.env;
        Self {
            level,
            rng,
            cfg,
            trunks: JavaSet::new(),
            foliage: JavaSet::new(),
            roots: JavaSet::new(),
            decorations: JavaSet::new(),
            env,
            leaves: env.tags.get("leaves").expect("leaves tag"),
            logs: env.tags.get("logs").expect("logs tag"),
            replaceable: env.tags.get("replaceable_by_trees").expect("replaceable_by_trees tag"),
            vine: env.blocks.block_by_name("vine").expect("vine"),
        }
    }

    pub fn get(&self, p: Pos) -> State {
        self.level.get(p.x, p.y, p.z)
    }

    pub fn is_air(&self, p: Pos) -> bool {
        self.env.blocks.is_air(self.get(p))
    }

    pub fn is_air_or_leaves(&self, p: Pos) -> bool {
        let s = self.get(p);
        self.env.blocks.is_air(s) || self.leaves.contains(self.env.blocks.block_of(s))
    }

    pub fn is_vine(&self, p: Pos) -> bool {
        self.env.blocks.block_of(self.get(p)) == self.vine
    }

    /// Air or a block trees may replace.
    pub fn valid_tree_pos(&self, p: Pos) -> bool {
        let s = self.get(p);
        self.env.blocks.is_air(s) || self.replaceable.contains(self.env.blocks.block_of(s))
    }

    /// The trunk placer's own notion of a free cell (some placers widen the valid set).
    pub fn trunk_valid(&self, p: Pos) -> bool {
        self.valid_tree_pos(p) || self.cfg.trunk_placer.grows_through(self.env.blocks.block_of(self.get(p)))
    }

    pub fn is_free(&self, p: Pos) -> bool {
        self.trunk_valid(p) || self.logs.contains(self.env.blocks.block_of(self.get(p)))
    }

    fn set_trunk(&mut self, p: Pos, s: State) {
        self.trunks.insert(p);
        self.level.set(p.x, p.y, p.z, s);
    }

    pub fn set_foliage(&mut self, p: Pos, s: State) {
        self.foliage.insert(p);
        self.level.set(p.x, p.y, p.z, s);
    }

    pub fn set_decoration(&mut self, p: Pos, s: State) {
        self.decorations.insert(p);
        self.level.set(p.x, p.y, p.z, s);
    }

    /// The block under the trunk (soil), when the provider has an answer.
    pub fn place_below_trunk(&mut self, p: Pos) {
        if let Some(s) = self.cfg.below_trunk.get_optional(self.level, self.rng, p.x, p.y, p.z) {
            self.set_trunk(p, s);
        }
    }

    /// A trunk block, optionally turned onto an axis.
    pub fn place_log(&mut self, p: Pos, axis: Option<&str>) -> bool {
        if !self.trunk_valid(p) {
            return false;
        }
        let mut s = self.cfg.trunk.get(self.level, self.rng, p.x, p.y, p.z);
        if let Some(a) = axis {
            if let Some(t) = self.env.blocks.with(s, "axis", a) {
                s = t;
            }
        }
        self.set_trunk(p, s);
        true
    }

    pub fn place_log_if_free(&mut self, p: Pos) {
        if self.is_free(p) {
            self.place_log(p, None);
        }
    }

    /// A leaf block when the cell is free and not already a persistent block.
    pub fn try_place_leaf(&mut self, p: Pos) -> bool {
        let blocks = &self.env.blocks;
        let here = self.get(p);
        let persistent = blocks.get(here, "persistent") == Some("true");
        if persistent || !self.valid_tree_pos(p) {
            return false;
        }
        let mut s = self.cfg.foliage.get(self.level, self.rng, p.x, p.y, p.z);
        if blocks.has_property(s, "waterlogged") {
            let source = blocks.fluid(here) == FluidKind::Water && blocks.fluid_is_source(here);
            s = blocks.with(s, "waterlogged", if source { "true" } else { "false" }).expect("waterlogged is boolean");
        }
        self.set_foliage(p, s);
        true
    }

    fn max_free_height(&self, max_tree_height: i32, tree_pos: Pos) -> i32 {
        for y in 0..=max_tree_height + 1 {
            let r = self.cfg.size.at_height(max_tree_height, y);
            for x in -r..=r {
                for z in -r..=r {
                    let p = tree_pos.offset(x, y, z);
                    if !self.is_free(p) || (!self.cfg.ignore_vines && self.is_vine(p)) {
                        return y - 2;
                    }
                }
            }
        }
        max_tree_height
    }

    fn do_place(&mut self, origin: Pos) -> bool {
        let cfg = self.cfg;
        let tree_height = cfg.trunk_placer.tree_height(self.rng);
        let foliage_height = cfg.foliage_placer.foliage_height(self.rng, tree_height);
        let trunk_height = tree_height - foliage_height;
        let leaf_radius = cfg.foliage_placer.foliage_radius(self.rng, trunk_height);
        let min_y = origin.y;
        let max_y = origin.y + tree_height + 1;
        if min_y < self.level.min_y + 1 || max_y > self.level.max_y() + 1 {
            return false;
        }
        let clipped = self.max_free_height(tree_height, origin);
        if clipped >= tree_height || cfg.size.min_clipped().is_some_and(|m| clipped >= m) {
            let attachments = cfg.trunk_placer.place(self, clipped, origin);
            for a in &attachments {
                cfg.foliage_placer.create(self, clipped, a, foliage_height, leaf_radius);
            }
            true
        } else {
            false
        }
    }

    /// Distances for leaves reachable from the trunk, in the order the reference visits them.
    fn update_leaves(&mut self, lo: Pos, hi: Pos) -> Vec<bool> {
        let (sx, sy, sz) = ((hi.x - lo.x + 1) as usize, (hi.y - lo.y + 1) as usize, (hi.z - lo.z + 1) as usize);
        let mut shape = vec![false; sx * sy * sz];
        let inside = |p: Pos| p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y && p.z >= lo.z && p.z <= hi.z;
        let idx = |p: Pos| ((p.x - lo.x) as usize * sy + (p.y - lo.y) as usize) * sz + (p.z - lo.z) as usize;
        for p in self.decorations.iter().chain(self.roots.iter()) {
            if inside(p) {
                shape[idx(p)] = true;
            }
        }
        let mut to_check: Vec<JavaSet> = (0..7).map(|_| JavaSet::new()).collect();
        for p in self.trunks.iter() {
            to_check[0].insert(p);
        }
        let blocks = &self.env.blocks;
        let prevents = self.env.tags.get("prevents_nearby_leaf_decay").expect("prevents_nearby_leaf_decay tag");
        let distance_at = |s: State| -> Option<i32> {
            if prevents.contains(blocks.block_of(s)) {
                Some(0)
            } else {
                blocks.get(s, "distance").and_then(|d| d.parse().ok())
            }
        };
        let mut smallest = 0usize;
        loop {
            while smallest >= 7 || !to_check[smallest].is_empty() {
                if smallest >= 7 {
                    return shape;
                }
                let p = to_check[smallest].pop_first().expect("non-empty list");
                if !inside(p) {
                    continue;
                }
                if smallest != 0 {
                    let s = self.get(p);
                    if let Some(t) = self.env.blocks.with(s, "distance", &smallest.to_string()) {
                        self.level.set(p.x, p.y, p.z, t);
                    }
                }
                shape[idx(p)] = true;
                for d in Dir::ALL {
                    let n = p.relative(d);
                    if inside(n) && !shape[idx(n)] {
                        if let Some(dist) = distance_at(self.get(n)) {
                            let new = dist.min(smallest as i32 + 1);
                            if new < 7 {
                                to_check[new as usize].insert(n);
                                smallest = smallest.min(new as usize);
                            }
                        }
                    }
                }
            }
            smallest += 1;
        }
    }
}

impl Run<'_, '_> {
    /// Re-evaluates the shape of every block on the outer faces of the placed region, in the
    /// reference's face order. Only plants whose survival depends on a neighbour react: a plant
    /// left without support (a double plant missing its other half, a bush under a new leaf)
    /// becomes air.
    fn update_edges(&mut self, lo: Pos, hi: Pos, shape: &[bool]) {
        let (sx, sy, sz) = ((hi.x - lo.x + 1) as usize, (hi.y - lo.y + 1) as usize, (hi.z - lo.z + 1) as usize);
        let full = |x: usize, y: usize, z: usize| shape[(x * sy + y) * sz + z];
        let mut faces: Vec<(Dir, Pos)> = Vec::new();
        let at = |x: usize, y: usize, z: usize| Pos::new(lo.x + x as i32, lo.y + y as i32, lo.z + z as i32);
        // Runs along z, then along y, then along x; each pass lists entering and leaving faces.
        for a in 0..sx {
            for b in 0..sy {
                let mut last = false;
                for c in 0..=sz {
                    let f = c != sz && full(a, b, c);
                    if !last && f {
                        faces.push((Dir::North, at(a, b, c)));
                    }
                    if last && !f {
                        faces.push((Dir::South, at(a, b, c - 1)));
                    }
                    last = f;
                }
            }
        }
        for a in 0..sz {
            for b in 0..sx {
                let mut last = false;
                for c in 0..=sy {
                    let f = c != sy && full(b, c, a);
                    if !last && f {
                        faces.push((Dir::Down, at(b, c, a)));
                    }
                    if last && !f {
                        faces.push((Dir::Up, at(b, c - 1, a)));
                    }
                    last = f;
                }
            }
        }
        for a in 0..sy {
            for b in 0..sz {
                let mut last = false;
                for c in 0..=sx {
                    let f = c != sx && full(c, a, b);
                    if !last && f {
                        faces.push((Dir::West, at(c, a, b)));
                    }
                    if last && !f {
                        faces.push((Dir::East, at(c - 1, a, b)));
                    }
                    last = f;
                }
            }
        }
        for (dir, pos) in faces {
            let neighbor = pos.relative(dir);
            let state = self.get(pos);
            let neighbor_state = self.get(neighbor);
            let new_state = self.shape_update(state, pos, dir, neighbor_state);
            if new_state != state {
                self.level.set(pos.x, pos.y, pos.z, new_state);
            }
            let new_neighbor = self.shape_update(neighbor_state, neighbor, dir.opposite(), new_state);
            if new_neighbor != neighbor_state {
                self.level.set(neighbor.x, neighbor.y, neighbor.z, new_neighbor);
            }
        }
    }

    /// Whether a vine face toward `dir` has something to hold: a block whose face is full.
    fn vine_attaches(&self, pos: Pos, dir: Dir) -> bool {
        super::misc::can_attach(self.level, pos, dir)
    }

    /// A vine keeps only the faces that still attach, or hang from the vine above; no faces
    /// left means air.
    fn vine_update(&self, state: State, pos: Pos) -> State {
        let blocks = &self.env.blocks;
        let mut s = state;
        let above = self.get(pos.above());
        if blocks.get(s, "up") == Some("true") {
            let ok = self.vine_attaches(pos, Dir::Up);
            s = blocks.with(s, "up", if ok { "true" } else { "false" }).expect("up");
        }
        for d in Dir::HORIZONTAL {
            let face = d.name();
            if blocks.get(s, face) == Some("true") {
                let ok = self.vine_attaches(pos, d)
                    || (blocks.block_of(above) == self.vine && blocks.get(above, face) == Some("true"));
                s = blocks.with(s, face, if ok { "true" } else { "false" }).expect("face");
            }
        }
        let any = ["up", "north", "east", "south", "west"].iter().any(|f| blocks.get(s, f) == Some("true"));
        if any { s } else { blocks.default_state(blocks.block_by_name("air").expect("air")) }
    }

    /// The state after a neighbour in `dir` changed to `neighbor`; plants without support turn
    /// to air, everything else is untouched.
    fn shape_update(&self, state: State, pos: Pos, dir: Dir, neighbor: State) -> State {
        let blocks = &self.env.blocks;
        let block = blocks.block_of(state);
        let air = || blocks.default_state(blocks.block_by_name("air").expect("air"));
        if block == self.vine {
            return if dir == Dir::Down { state } else { self.vine_update(state, pos) };
        }
        match self.env.survive.kind(block) {
            Kind::DoubleVegetation => {
                let lower = blocks.get(state, "half") == Some("lower");
                let vertical = matches!(dir, Dir::Up | Dir::Down);
                let toward_other = lower == (dir == Dir::Up);
                let intact = blocks.block_of(neighbor) == block && blocks.get(neighbor, "half") != blocks.get(state, "half");
                if vertical && toward_other && !intact {
                    return air();
                }
                if !survive::can_survive(self.level, state, pos.x, pos.y, pos.z) { air() } else { state }
            }
            Kind::Vegetation | Kind::DryVegetation | Kind::Azalea => {
                if survive::can_survive(self.level, state, pos.x, pos.y, pos.z) { state } else { air() }
            }
            _ => state,
        }
    }
}

/// Places a tree at `origin`; returns whether one was placed.
pub fn place_tree(cfg: &TreeConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    if !cfg.unsupported.is_empty() {
        return false;
    }
    let mut run = Run::new(level, rng, cfg);
    let placed = run.do_place(origin);
    if !placed || (run.trunks.is_empty() && run.foliage.is_empty()) {
        return false;
    }
    for d in &cfg.decorators {
        d.place(&mut run);
    }
    let mut lo = Pos::new(i32::MAX, i32::MAX, i32::MAX);
    let mut hi = Pos::new(i32::MIN, i32::MIN, i32::MIN);
    for p in run.roots.iter().chain(run.trunks.iter()).chain(run.foliage.iter()).chain(run.decorations.iter()) {
        lo = Pos::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
        hi = Pos::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
    }
    let shape = run.update_leaves(lo, hi);
    run.update_edges(lo, hi, &shape);
    true
}

pub(super) fn sorted_by_y(set: &JavaSet) -> Vec<Pos> {
    let mut v: Vec<Pos> = set.iter().collect();
    v.sort_by_key(|p| p.y);
    v
}
