//! Trunk placers: the shape of the log column and where foliage attaches.

use lodestone_worldgen_core::math;
use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use super::foliage::Attachment;
use super::{Run, int_provider, plain_uniform};
use crate::blocks::{BlockId, Dir};
use crate::env::Env;
use crate::json::{Res, float, get, int, int_or, type_of};
use crate::pos::{Pos, Rng};
use crate::provider::IntProvider;

#[derive(Clone, Debug)]
pub enum TrunkKind {
    Straight,
    Forking,
    Giant,
    MegaJungle,
    DarkOak,
    Fancy,
    Bending { min_height_for_leaves: i32, bend_length: IntProvider },
    UpwardsBranching { extra_branch_steps: IntProvider, branch_probability: f32, extra_branch_length: IntProvider, grow_through: Vec<BlockId> },
    Cherry { branch_count: IntProvider, horizontal_length: IntProvider, start_offset: (i32, i32), end_offset: IntProvider },
    Poplar { above_branches: IntProvider, branch_amount: IntProvider },
}

#[derive(Clone, Debug)]
pub struct TrunkPlacer {
    pub base: i32,
    pub rand_a: i32,
    pub rand_b: i32,
    pub kind: TrunkKind,
}

fn horizontal(rng: &mut Rng) -> Dir {
    Dir::HORIZONTAL[rng.next_int_bounded(4) as usize]
}

/// The reference's list shuffle: swap the last slot with a random earlier one, shrinking.
pub(crate) fn shuffle<T>(list: &mut [T], rng: &mut Rng) {
    let mut i = list.len();
    while i > 1 {
        let swap_to = rng.next_int_bounded(i as i32) as usize;
        list.swap(i - 1, swap_to);
        i -= 1;
    }
}

impl TrunkPlacer {
    pub fn parse(env: &Env, v: &Value, ctx: &str, unsupported: &mut Vec<String>) -> Res<Self> {
        let t = type_of(v, ctx)?;
        let kind = match t {
            "straight_trunk_placer" => TrunkKind::Straight,
            "forking_trunk_placer" => TrunkKind::Forking,
            "giant_trunk_placer" => TrunkKind::Giant,
            "mega_jungle_trunk_placer" => TrunkKind::MegaJungle,
            "dark_oak_trunk_placer" => TrunkKind::DarkOak,
            "fancy_trunk_placer" => TrunkKind::Fancy,
            "bending_trunk_placer" => TrunkKind::Bending {
                min_height_for_leaves: int_or(v, "min_height_for_leaves", 1, ctx)?,
                bend_length: int_provider(v, "bend_length", ctx)?,
            },
            "upwards_branching_trunk_placer" => {
                let blocks = super::block_list(env, get(v, "can_grow_through", ctx)?, ctx)?;
                TrunkKind::UpwardsBranching {
                    extra_branch_steps: int_provider(v, "extra_branch_steps", ctx)?,
                    branch_probability: float(v, "place_branch_per_log_probability", ctx)?,
                    extra_branch_length: int_provider(v, "extra_branch_length", ctx)?,
                    grow_through: blocks,
                }
            }
            "cherry_trunk_placer" => TrunkKind::Cherry {
                branch_count: int_provider(v, "branch_count", ctx)?,
                horizontal_length: int_provider(v, "branch_horizontal_length", ctx)?,
                start_offset: plain_uniform(v, "branch_start_offset_from_top", ctx)?,
                end_offset: int_provider(v, "branch_end_offset_from_top", ctx)?,
            },
            "poplar_trunk_placer" => TrunkKind::Poplar {
                above_branches: int_provider(v, "trunk_height_above_branches", ctx)?,
                branch_amount: int_provider(v, "branch_amount", ctx)?,
            },
            other => {
                unsupported.push(format!("trunk_placer {other}"));
                TrunkKind::Straight
            }
        };
        Ok(Self { base: int(v, "base_height", ctx)?, rand_a: int(v, "height_rand_a", ctx)?, rand_b: int(v, "height_rand_b", ctx)?, kind })
    }

    pub fn tree_height(&self, rng: &mut Rng) -> i32 {
        self.base + rng.next_int_bounded(self.rand_a + 1) + rng.next_int_bounded(self.rand_b + 1)
    }

    /// Whether the placer treats the block as free to grow through beyond the usual set.
    pub fn grows_through(&self, b: BlockId) -> bool {
        matches!(&self.kind, TrunkKind::UpwardsBranching { grow_through, .. } if grow_through.contains(&b))
    }

    pub fn place(&self, run: &mut Run<'_, '_>, tree_height: i32, origin: Pos) -> Vec<Attachment> {
        match &self.kind {
            TrunkKind::Straight => {
                run.place_below_trunk(origin.below());
                for y in 0..tree_height {
                    run.place_log(origin.offset(0, y, 0), None);
                }
                vec![Attachment::new(origin.offset(0, tree_height, 0), 0, false)]
            }
            TrunkKind::Forking => forking(run, tree_height, origin),
            TrunkKind::Giant => giant(run, tree_height, origin),
            TrunkKind::MegaJungle => mega_jungle(run, tree_height, origin),
            TrunkKind::DarkOak => dark_oak(run, tree_height, origin),
            TrunkKind::Fancy => fancy(run, tree_height, origin),
            TrunkKind::Bending { min_height_for_leaves, bend_length } => bending(run, tree_height, origin, *min_height_for_leaves, bend_length),
            TrunkKind::UpwardsBranching { extra_branch_steps, branch_probability, extra_branch_length, .. } => {
                upwards_branching(run, tree_height, origin, extra_branch_steps, *branch_probability, extra_branch_length)
            }
            TrunkKind::Cherry { branch_count, horizontal_length, start_offset, end_offset } => {
                cherry(run, tree_height, origin, branch_count, horizontal_length, *start_offset, end_offset)
            }
            TrunkKind::Poplar { above_branches, branch_amount } => poplar(run, tree_height, origin, above_branches, branch_amount),
        }
    }
}

fn forking(run: &mut Run<'_, '_>, tree_height: i32, origin: Pos) -> Vec<Attachment> {
    run.place_below_trunk(origin.below());
    let mut out = Vec::new();
    let lean = horizontal(run.rng);
    let lean_height = tree_height - run.rng.next_int_bounded(4) - 1;
    let mut lean_steps = 3 - run.rng.next_int_bounded(3);
    let (mut tx, mut tz) = (origin.x, origin.z);
    let mut ey = None;
    for yo in 0..tree_height {
        let yy = origin.y + yo;
        if yo >= lean_height && lean_steps > 0 {
            let (dx, _, dz) = lean.step();
            tx += dx;
            tz += dz;
            lean_steps -= 1;
        }
        if run.place_log(Pos::new(tx, yy, tz), None) {
            ey = Some(yy + 1);
        }
    }
    if let Some(ey) = ey {
        out.push(Attachment::new(Pos::new(tx, ey, tz), 1, false));
    }
    tx = origin.x;
    tz = origin.z;
    let branch = horizontal(run.rng);
    if branch != lean {
        let branch_pos = lean_height - run.rng.next_int_bounded(2) - 1;
        let mut branch_steps = 1 + run.rng.next_int_bounded(3);
        ey = None;
        let mut yo = branch_pos;
        while yo < tree_height && branch_steps > 0 {
            if yo >= 1 {
                let yy = origin.y + yo;
                let (dx, _, dz) = branch.step();
                tx += dx;
                tz += dz;
                if run.place_log(Pos::new(tx, yy, tz), None) {
                    ey = Some(yy + 1);
                }
            }
            yo += 1;
            branch_steps -= 1;
        }
        if let Some(ey) = ey {
            out.push(Attachment::new(Pos::new(tx, ey, tz), 0, false));
        }
    }
    out
}

fn giant_base(run: &mut Run<'_, '_>, tree_height: i32, origin: Pos) {
    let below = origin.below();
    run.place_below_trunk(below);
    run.place_below_trunk(below.offset(1, 0, 0));
    run.place_below_trunk(below.offset(0, 0, 1));
    run.place_below_trunk(below.offset(1, 0, 1));
    for hh in 0..tree_height {
        run.place_log_if_free(origin.offset(0, hh, 0));
        if hh < tree_height - 1 {
            run.place_log_if_free(origin.offset(1, hh, 0));
            run.place_log_if_free(origin.offset(1, hh, 1));
            run.place_log_if_free(origin.offset(0, hh, 1));
        }
    }
}

fn giant(run: &mut Run<'_, '_>, tree_height: i32, origin: Pos) -> Vec<Attachment> {
    giant_base(run, tree_height, origin);
    vec![Attachment::new(origin.offset(0, tree_height, 0), 0, true)]
}

fn mega_jungle(run: &mut Run<'_, '_>, tree_height: i32, origin: Pos) -> Vec<Attachment> {
    let mut out = giant(run, tree_height, origin);
    let mut branch_height = tree_height - 2 - run.rng.next_int_bounded(4);
    while branch_height > tree_height / 2 {
        let angle = run.rng.next_float() * (std::f64::consts::PI * 2.0) as f32;
        let (mut bx, mut bz) = (0, 0);
        for b in 0..5 {
            bx = (1.5f32 + math::cos(f64::from(angle)) * b as f32) as i32;
            bz = (1.5f32 + math::sin(f64::from(angle)) * b as f32) as i32;
            run.place_log(origin.offset(bx, branch_height - 3 + b / 2, bz), None);
        }
        out.push(Attachment::new(origin.offset(bx, branch_height, bz), -2, false));
        branch_height -= 2 + run.rng.next_int_bounded(4);
    }
    out
}

fn dark_oak(run: &mut Run<'_, '_>, tree_height: i32, origin: Pos) -> Vec<Attachment> {
    let mut out = Vec::new();
    let below = origin.below();
    run.place_below_trunk(below);
    run.place_below_trunk(below.offset(1, 0, 0));
    run.place_below_trunk(below.offset(0, 0, 1));
    run.place_below_trunk(below.offset(1, 0, 1));
    let lean = horizontal(run.rng);
    let lean_height = tree_height - run.rng.next_int_bounded(4);
    let mut lean_steps = 2 - run.rng.next_int_bounded(3);
    let (x, y, z) = (origin.x, origin.y, origin.z);
    let (mut tx, mut tz) = (x, z);
    let ey = y + tree_height - 1;
    for dy in 0..tree_height {
        if dy >= lean_height && lean_steps > 0 {
            let (dx, _, dz) = lean.step();
            tx += dx;
            tz += dz;
            lean_steps -= 1;
        }
        let p = Pos::new(tx, y + dy, tz);
        if run.is_air_or_leaves(p) {
            run.place_log(p, None);
            run.place_log(p.offset(1, 0, 0), None);
            run.place_log(p.offset(0, 0, 1), None);
            run.place_log(p.offset(1, 0, 1), None);
        }
    }
    out.push(Attachment::new(Pos::new(tx, ey, tz), 0, true));
    for ox in -1..=2 {
        for oz in -1..=2 {
            if (ox < 0 || ox > 1 || oz < 0 || oz > 1) && run.rng.next_int_bounded(3) <= 0 {
                let length = run.rng.next_int_bounded(3) + 2;
                for by in 0..length {
                    run.place_log(Pos::new(x + ox, ey - by - 1, z + oz), None);
                }
                out.push(Attachment::new(Pos::new(x + ox, ey, z + oz), 0, false));
            }
        }
    }
    out
}

fn bending(run: &mut Run<'_, '_>, tree_height: i32, origin: Pos, min_leaves: i32, bend: &IntProvider) -> Vec<Attachment> {
    let dir = horizontal(run.rng);
    let log_height = tree_height - 1;
    let mut pos = origin;
    run.place_below_trunk(pos.below());
    let mut out = Vec::new();
    for i in 0..=log_height {
        if i + 1 >= log_height + run.rng.next_int_bounded(2) {
            pos = pos.relative(dir);
        }
        if run.valid_tree_pos(pos) {
            run.place_log(pos, None);
        }
        if i >= min_leaves {
            out.push(Attachment::new(pos, 0, false));
        }
        pos = pos.above();
    }
    let len = bend.sample(run.rng);
    for _ in 0..=len {
        if run.valid_tree_pos(pos) {
            run.place_log(pos, None);
        }
        out.push(Attachment::new(pos, 0, false));
        pos = pos.relative(dir);
    }
    out
}

fn upwards_branching(
    run: &mut Run<'_, '_>,
    tree_height: i32,
    origin: Pos,
    steps: &IntProvider,
    probability: f32,
    length: &IntProvider,
) -> Vec<Attachment> {
    let mut out = Vec::new();
    let mut log;
    for h in 0..tree_height {
        let current = origin.y + h;
        log = Pos::new(origin.x, current, origin.z);
        if run.place_log(log, None) && h < tree_height - 1 && run.rng.next_float() < probability {
            let dir = horizontal(run.rng);
            let len = length.sample(run.rng);
            let branch_pos = 0.max(len - length.sample(run.rng) - 1);
            let branch_steps = steps.sample(run.rng);
            place_branch(run, tree_height, &mut out, &mut log, current, dir, branch_pos, branch_steps);
        }
        if h == tree_height - 1 {
            log = Pos::new(origin.x, current + 1, origin.z);
            out.push(Attachment::new(log, 0, false));
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn place_branch(
    run: &mut Run<'_, '_>,
    tree_height: i32,
    out: &mut Vec<Attachment>,
    log: &mut Pos,
    current: i32,
    dir: Dir,
    branch_pos: i32,
    mut steps: i32,
) {
    let mut along = current + branch_pos;
    let (mut lx, mut lz) = (log.x, log.z);
    let mut idx = branch_pos;
    while idx < tree_height && steps > 0 {
        if idx >= 1 {
            let h = current + idx;
            let (dx, _, dz) = dir.step();
            lx += dx;
            lz += dz;
            along = h;
            *log = Pos::new(lx, h, lz);
            if run.place_log(*log, None) {
                along += 1;
            }
            out.push(Attachment::new(*log, 0, false));
        }
        idx += 1;
        steps -= 1;
    }
    if along - current > 1 {
        let foliage = Pos::new(lx, along, lz);
        out.push(Attachment::new(foliage, 0, false));
        out.push(Attachment::new(foliage.offset(0, -2, 0), 0, false));
    }
}

fn cherry(
    run: &mut Run<'_, '_>,
    tree_height: i32,
    origin: Pos,
    branch_count: &IntProvider,
    horizontal_length: &IntProvider,
    start: (i32, i32),
    end_offset: &IntProvider,
) -> Vec<Attachment> {
    run.place_below_trunk(origin.below());
    let uniform = |rng: &mut Rng, lo: i32, hi: i32| rng.next_int_bounded(hi - lo + 1) + lo;
    let first = 0.max(tree_height - 1 + uniform(run.rng, start.0, start.1));
    let mut second = 0.max(tree_height - 1 + uniform(run.rng, start.0, start.1 - 1));
    if second >= first {
        second += 1;
    }
    let count = branch_count.sample(run.rng);
    let middle = count == 3;
    let both = count >= 2;
    let trunk_height = if middle {
        tree_height
    } else if both {
        first.max(second) + 1
    } else {
        first + 1
    };
    for y in 0..trunk_height {
        run.place_log(origin.offset(0, y, 0), None);
    }
    let mut out = Vec::new();
    if middle {
        out.push(Attachment::new(origin.offset(0, trunk_height, 0), 0, false));
    }
    let tree_dir = horizontal(run.rng);
    out.push(cherry_branch(run, tree_height, origin, tree_dir, first, first < trunk_height - 1, horizontal_length, end_offset));
    if both {
        out.push(cherry_branch(run, tree_height, origin, tree_dir.opposite(), second, second < trunk_height - 1, horizontal_length, end_offset));
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn cherry_branch(
    run: &mut Run<'_, '_>,
    tree_height: i32,
    origin: Pos,
    dir: Dir,
    offset_from_origin: i32,
    middle_continues_upwards: bool,
    horizontal_length: &IntProvider,
    end_offset: &IntProvider,
) -> Attachment {
    let mut log = origin.offset(0, offset_from_origin, 0);
    let end_y_offset = tree_height - 1 + end_offset.sample(run.rng);
    let extend_away = middle_continues_upwards || end_y_offset < offset_from_origin;
    let distance = horizontal_length.sample(run.rng) + i32::from(extend_away);
    let end = origin.relative_n(dir, distance).offset(0, end_y_offset, 0);
    let axis = dir.axis_name();
    for _ in 0..if extend_away { 2 } else { 1 } {
        log = log.relative(dir);
        run.place_log(log, Some(axis));
    }
    let vertical = if end.y > log.y { Dir::Up } else { Dir::Down };
    loop {
        let distance = (log.x - end.x).abs() + (log.y - end.y).abs() + (log.z - end.z).abs();
        if distance == 0 {
            return Attachment::new(end.above(), 0, false);
        }
        let chance = (end.y - log.y).abs() as f32 / distance as f32;
        let grow_vertically = run.rng.next_float() < chance;
        log = log.relative(if grow_vertically { vertical } else { dir });
        run.place_log(log, if grow_vertically { None } else { Some(axis) });
    }
}

fn poplar(run: &mut Run<'_, '_>, tree_height: i32, origin: Pos, above: &IntProvider, amount: &IntProvider) -> Vec<Attachment> {
    run.place_below_trunk(origin.below());
    let up_to_branches = tree_height - above.sample(run.rng);
    for y in 0..tree_height {
        run.place_log(origin.offset(0, y, 0), None);
        let mut dirs = Dir::ALL;
        shuffle(&mut dirs, run.rng);
        let dirs: Vec<Dir> = dirs.into_iter().filter(|d| !matches!(d, Dir::Up | Dir::Down)).collect();
        if up_to_branches - 1 == y {
            let branches = amount.sample(run.rng);
            for x in 0..branches {
                let d = dirs[x as usize];
                run.place_log(origin.offset(0, y, 0).relative(d), Some(d.axis_name()));
            }
        }
    }
    vec![Attachment::new(origin.offset(0, up_to_branches, 0), 0, false)]
}

fn tree_shape(height: i32, y: i32) -> f32 {
    if (y as f32) < height as f32 * 0.3f32 {
        return -1.0;
    }
    let radius = height as f32 / 2.0f32;
    let adjacent = radius - y as f32;
    let mut distance = ((radius * radius - adjacent * adjacent) as f64).sqrt() as f32;
    if adjacent == 0.0 {
        distance = radius;
    } else if adjacent.abs() >= radius {
        return 0.0;
    }
    distance * 0.5f32
}

fn log_axis(start: Pos, p: Pos) -> &'static str {
    let (xd, zd) = ((p.x - start.x).abs(), (p.z - start.z).abs());
    let max = xd.max(zd);
    if max > 0 {
        if xd == max { "x" } else { "z" }
    } else {
        "y"
    }
}

/// Walks a straight limb; places logs along it, or only checks that every cell is free.
fn make_limb(run: &mut Run<'_, '_>, start: Pos, end: Pos, place: bool) -> bool {
    if !place && start == end {
        return true;
    }
    let (ddx, ddy, ddz) = (end.x - start.x, end.y - start.y, end.z - start.z);
    let steps = ddx.abs().max(ddy.abs()).max(ddz.abs());
    let (dx, dy, dz) = (ddx as f32 / steps as f32, ddy as f32 / steps as f32, ddz as f32 / steps as f32);
    for i in 0..=steps {
        let f = |d: f32| (0.5f32 + i as f32 * d).floor() as i32;
        let p = start.offset(f(dx), f(dy), f(dz));
        if place {
            run.place_log(p, Some(log_axis(start, p)));
        } else if !run.is_free(p) {
            return false;
        }
    }
    true
}

fn fancy(run: &mut Run<'_, '_>, tree_height: i32, origin: Pos) -> Vec<Attachment> {
    let height = tree_height + 2;
    let trunk_height = math::floor(f64::from(height) * 0.618);
    run.place_below_trunk(origin.below());
    let clusters_per_y = 1.min(math::floor(1.382 + (f64::from(height) / 13.0).powi(2)));
    let trunk_top = origin.y + trunk_height;
    let mut relative_y = height - 5;
    let mut coords: Vec<(Pos, i32)> = vec![(origin.offset(0, relative_y, 0), trunk_top)];
    while relative_y >= 0 {
        let shape = tree_shape(height, relative_y);
        if !(shape < 0.0) {
            for _ in 0..clusters_per_y {
                let radius = 1.0 * f64::from(shape) * (f64::from(run.rng.next_float()) + 0.328);
                let angle = f64::from(run.rng.next_float() * 2.0f32) * std::f64::consts::PI;
                let x = radius * angle.sin() + 0.5;
                let z = radius * angle.cos() + 0.5;
                let start = origin.offset(math::floor(x), relative_y - 1, math::floor(z));
                let end = start.offset(0, 5, 0);
                if make_limb(run, start, end, false) {
                    let (dx, dz) = (origin.x - start.x, origin.z - start.z);
                    let branch_height = f64::from(start.y) - f64::from(dx * dx + dz * dz).sqrt() * 0.381;
                    let branch_top = if branch_height > f64::from(trunk_top) { trunk_top } else { branch_height as i32 };
                    let base = Pos::new(origin.x, branch_top, origin.z);
                    if make_limb(run, base, start, false) {
                        coords.push((start, base.y));
                    }
                }
            }
        }
        relative_y -= 1;
    }
    make_limb(run, origin, origin.offset(0, trunk_height, 0), true);
    let trim = |local_y: i32| f64::from(local_y) >= f64::from(height) * 0.2;
    for &(pos, branch_base) in &coords {
        let base = Pos::new(origin.x, branch_base, origin.z);
        if base != pos && trim(branch_base - origin.y) {
            make_limb(run, base, pos, true);
        }
    }
    coords.iter().filter(|(_, b)| trim(b - origin.y)).map(|(p, _)| Attachment::new(*p, 0, false)).collect()
}
