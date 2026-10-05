//! Foliage placers: the leaf shapes around each attachment point the trunk produced.

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::Value;

use super::{Run, int_provider};
use crate::blocks::Dir;
use crate::json::{Res, float, int, type_of};
use crate::pos::{Pos, Rng};
use crate::provider::IntProvider;

/// Where foliage hangs off the trunk.
#[derive(Clone, Copy, Debug)]
pub struct Attachment {
    pub pos: Pos,
    pub radius_offset: i32,
    pub height_offset: i32,
    pub size_x: i32,
    pub size_z: i32,
}

impl Attachment {
    #[must_use]
    pub fn new(pos: Pos, radius_offset: i32, double_trunk: bool) -> Self {
        let size = if double_trunk { 2 } else { 1 };
        Self { pos, radius_offset, height_offset: 0, size_x: size, size_z: size }
    }

    fn double_trunk(&self) -> bool {
        self.size_x == 2 && self.size_z == 2
    }
}

#[derive(Clone, Debug)]
pub enum FoliageKind {
    Blob { height: i32 },
    Bush { height: i32 },
    Fancy { height: i32 },
    Spruce { trunk_height: IntProvider },
    Pine { height: IntProvider },
    Acacia,
    MegaPine { crown_height: IntProvider },
    MegaJungle { height: i32 },
    DarkOak,
    RandomSpread { foliage_height: IntProvider, attempts: i32 },
    Cherry { height: IntProvider, wide_bottom_hole: f32, corner_hole: f32, hanging: f32, hanging_extension: f32 },
    Poplar { height: IntProvider, side_hole: f32 },
}

#[derive(Clone, Debug)]
pub struct FoliagePlacer {
    pub radius: IntProvider,
    pub offset: IntProvider,
    pub kind: FoliageKind,
}

impl FoliagePlacer {
    pub fn parse(v: &Value, ctx: &str, unsupported: &mut Vec<String>) -> Res<Self> {
        let t = type_of(v, ctx)?;
        let kind = match t {
            "blob_foliage_placer" => FoliageKind::Blob { height: int(v, "height", ctx)? },
            "bush_foliage_placer" => FoliageKind::Bush { height: int(v, "height", ctx)? },
            "fancy_foliage_placer" => FoliageKind::Fancy { height: int(v, "height", ctx)? },
            "spruce_foliage_placer" => FoliageKind::Spruce { trunk_height: int_provider(v, "trunk_height", ctx)? },
            "pine_foliage_placer" => FoliageKind::Pine { height: int_provider(v, "height", ctx)? },
            "acacia_foliage_placer" => FoliageKind::Acacia,
            "mega_pine_foliage_placer" => FoliageKind::MegaPine { crown_height: int_provider(v, "crown_height", ctx)? },
            "jungle_foliage_placer" => FoliageKind::MegaJungle { height: int(v, "height", ctx)? },
            "dark_oak_foliage_placer" => FoliageKind::DarkOak,
            "random_spread_foliage_placer" => FoliageKind::RandomSpread {
                foliage_height: int_provider(v, "foliage_height", ctx)?,
                attempts: int(v, "leaf_placement_attempts", ctx)?,
            },
            "cherry_foliage_placer" => FoliageKind::Cherry {
                height: int_provider(v, "height", ctx)?,
                wide_bottom_hole: float(v, "wide_bottom_layer_hole_chance", ctx)?,
                corner_hole: float(v, "corner_hole_chance", ctx)?,
                hanging: float(v, "hanging_leaves_chance", ctx)?,
                hanging_extension: float(v, "hanging_leaves_extension_chance", ctx)?,
            },
            "poplar_foliage_placer" => {
                FoliageKind::Poplar { height: int_provider(v, "height", ctx)?, side_hole: float(v, "side_hole_chance", ctx)? }
            }
            other => {
                unsupported.push(format!("foliage_placer {other}"));
                FoliageKind::Acacia
            }
        };
        Ok(Self { radius: int_provider(v, "radius", ctx)?, offset: int_provider(v, "offset", ctx)?, kind })
    }

    pub fn foliage_height(&self, rng: &mut Rng, tree_height: i32) -> i32 {
        match &self.kind {
            FoliageKind::Blob { height } | FoliageKind::Bush { height } | FoliageKind::Fancy { height } | FoliageKind::MegaJungle { height } => *height,
            FoliageKind::Spruce { trunk_height } => 4.max(tree_height - trunk_height.sample(rng)),
            FoliageKind::Pine { height } => height.sample(rng),
            FoliageKind::Acacia => 0,
            FoliageKind::MegaPine { crown_height } => crown_height.sample(rng),
            FoliageKind::DarkOak => 4,
            FoliageKind::RandomSpread { foliage_height, .. } => foliage_height.sample(rng),
            FoliageKind::Cherry { height, .. } | FoliageKind::Poplar { height, .. } => height.sample(rng),
        }
    }

    pub fn foliage_radius(&self, rng: &mut Rng, trunk_height: i32) -> i32 {
        let base = self.radius.sample(rng);
        if matches!(self.kind, FoliageKind::Pine { .. }) {
            base + rng.next_int_bounded((trunk_height + 1).max(1))
        } else {
            base
        }
    }

    /// Whether the cell at the unsigned offsets is left empty.
    fn skip(&self, rng: &mut Rng, dx: i32, y: i32, dz: i32, r: i32, double: bool) -> bool {
        match &self.kind {
            FoliageKind::Blob { .. } => dx == r && dz == r && (rng.next_int_bounded(2) == 0 || y == 0),
            FoliageKind::Bush { .. } => dx == r && dz == r && rng.next_int_bounded(2) == 0,
            FoliageKind::Fancy { .. } => {
                let (a, b) = (dx as f32 + 0.5f32, dz as f32 + 0.5f32);
                a * a + b * b > (r * r) as f32
            }
            FoliageKind::Spruce { .. } | FoliageKind::Pine { .. } => dx == r && dz == r && r > 0,
            FoliageKind::Acacia => {
                if y == 0 {
                    (dx > 1 || dz > 1) && dx != 0 && dz != 0
                } else {
                    dx == r && dz == r && r > 0
                }
            }
            FoliageKind::MegaPine { .. } | FoliageKind::MegaJungle { .. } => dx + dz >= 7 || dx * dx + dz * dz > r * r,
            FoliageKind::DarkOak => {
                if y == -1 && !double {
                    dx == r && dz == r
                } else {
                    y == 1 && dx + dz > r * 2 - 2
                }
            }
            FoliageKind::RandomSpread { .. } => false,
            FoliageKind::Cherry { wide_bottom_hole, corner_hole, .. } => {
                if y == -1 && (dx == r || dz == r) && rng.next_float() < *wide_bottom_hole {
                    return true;
                }
                let corner = dx == r && dz == r;
                if r > 2 {
                    corner || (dx + dz > r * 2 - 2 && rng.next_float() < *corner_hole)
                } else {
                    corner && rng.next_float() < *corner_hole
                }
            }
            FoliageKind::Poplar { .. } => unreachable!("poplar rows are placed by their own routine"),
        }
    }

    fn skip_signed(&self, rng: &mut Rng, dx: i32, y: i32, dz: i32, r: i32, double: bool) -> bool {
        if matches!(self.kind, FoliageKind::DarkOak) && y == 0 && double && (dx == -r || dx >= r) && (dz == -r || dz >= r) {
            return true;
        }
        let (mx, mz) = if double { (dx.abs().min((dx - 1).abs()), dz.abs().min((dz - 1).abs())) } else { (dx.abs(), dz.abs()) };
        self.skip(rng, mx, y, mz, r, double)
    }

    fn row(&self, run: &mut Run<'_, '_>, origin: Pos, r: i32, y: i32, double: bool) {
        let off = i32::from(double);
        for dx in -r..=r + off {
            for dz in -r..=r + off {
                if !self.skip_signed(run.rng, dx, y, dz, r, double) {
                    run.try_place_leaf(origin.offset(dx, y, dz));
                }
            }
        }
    }

    pub fn create(&self, run: &mut Run<'_, '_>, tree_height: i32, a: &Attachment, foliage_height: i32, leaf_radius: i32) {
        let offset = self.offset.sample(run.rng);
        let double = a.double_trunk();
        let with_offset = foliage_height + a.height_offset;
        match &self.kind {
            FoliageKind::Blob { .. } => {
                for yo in (offset - with_offset..=offset).rev() {
                    let r = 0.max(leaf_radius + a.radius_offset - 1 - yo / 2);
                    self.row(run, a.pos, r, yo, double);
                }
            }
            FoliageKind::Bush { .. } => {
                for yo in (offset - with_offset..=offset).rev() {
                    let r = leaf_radius + a.radius_offset - 1 - yo;
                    self.row(run, a.pos, r, yo, double);
                }
            }
            FoliageKind::Fancy { .. } => {
                for yo in (offset - foliage_height..=offset).rev() {
                    let r = leaf_radius + i32::from(yo != offset && yo != offset - foliage_height);
                    self.row(run, a.pos, r, yo, double);
                }
            }
            FoliageKind::Spruce { .. } => {
                let mut r = run.rng.next_int_bounded(2);
                let (mut max_r, mut min_r) = (1, 0);
                for yo in (-with_offset..=offset).rev() {
                    self.row(run, a.pos, r, yo, double);
                    if r >= max_r {
                        r = min_r;
                        min_r = 1;
                        max_r = (max_r + 1).min(leaf_radius + a.radius_offset);
                    } else {
                        r += 1;
                    }
                }
            }
            FoliageKind::Pine { .. } => {
                let mut r = 0;
                for yo in (offset - with_offset..=offset).rev() {
                    self.row(run, a.pos, r, yo, double);
                    if r >= 1 && yo == offset - with_offset + 1 {
                        r -= 1;
                    } else if r < leaf_radius + a.radius_offset {
                        r += 1;
                    }
                }
            }
            FoliageKind::Acacia => {
                let pos = a.pos.offset(0, offset, 0);
                self.row(run, pos, leaf_radius + a.radius_offset, -1 - with_offset, double);
                self.row(run, pos, leaf_radius - 1, -with_offset, double);
                self.row(run, pos, leaf_radius + a.radius_offset - 1, 0, double);
            }
            FoliageKind::MegaPine { .. } => {
                let mut prev = 0;
                for yy in a.pos.y - with_offset + offset..=a.pos.y + offset {
                    let yo = a.pos.y - yy;
                    let smooth = leaf_radius + a.radius_offset + (yo as f32 / with_offset as f32 * 3.5f32).floor() as i32;
                    let jagged = if yo > 0 && smooth == prev && (yy & 1) == 0 { smooth + 1 } else { smooth };
                    self.row(run, Pos::new(a.pos.x, yy, a.pos.z), jagged, 0, double);
                    prev = smooth;
                }
            }
            FoliageKind::MegaJungle { .. } => {
                let leaf_height = (if double { foliage_height } else { 1 + run.rng.next_int_bounded(2) }) + a.height_offset;
                for yo in (offset - leaf_height..=offset).rev() {
                    let r = leaf_radius + a.radius_offset + 1 - yo;
                    self.row(run, a.pos, r, yo, double);
                }
            }
            FoliageKind::DarkOak => {
                let pos = a.pos.offset(0, offset, 0);
                if double {
                    self.row(run, pos, leaf_radius + 2, -1, double);
                    self.row(run, pos, leaf_radius + 3, 0, double);
                    self.row(run, pos, leaf_radius + 2, 1, double);
                    if run.rng.next_bool() {
                        self.row(run, pos, leaf_radius, 2, double);
                    }
                } else {
                    self.row(run, pos, leaf_radius + 2, -1, double);
                    self.row(run, pos, leaf_radius + 1, 0, double);
                }
            }
            FoliageKind::RandomSpread { attempts, foliage_height: fh } => {
                let _ = fh;
                for _ in 0..*attempts {
                    let dx = run.rng.next_int_bounded(leaf_radius) - run.rng.next_int_bounded(leaf_radius);
                    let dy = run.rng.next_int_bounded(foliage_height) - run.rng.next_int_bounded(foliage_height);
                    let dz = run.rng.next_int_bounded(leaf_radius) - run.rng.next_int_bounded(leaf_radius);
                    run.try_place_leaf(a.pos.offset(dx, dy, dz));
                }
            }
            FoliageKind::Cherry { hanging, hanging_extension, .. } => {
                let pos = a.pos.offset(0, offset, 0);
                let r = leaf_radius + a.radius_offset - 1;
                self.row(run, pos, r - 2, with_offset - 3, double);
                self.row(run, pos, r - 1, with_offset - 4, double);
                for y in (0..=with_offset - 5).rev() {
                    self.row(run, pos, r, y, double);
                }
                self.row_with_hanging(run, pos, r, -1, double, *hanging, *hanging_extension);
                self.row_with_hanging(run, pos, r - 1, -2, double, *hanging, *hanging_extension);
            }
            FoliageKind::Poplar { side_hole, .. } => poplar(run, a, offset, with_offset, leaf_radius, *side_hole),
        }
        let _ = tree_height;
    }

    #[allow(clippy::too_many_arguments)]
    fn row_with_hanging(&self, run: &mut Run<'_, '_>, origin: Pos, r: i32, y: i32, double: bool, chance: f32, ext_chance: f32) {
        self.row(run, origin, r, y, double);
        let off = i32::from(double);
        let log = origin.below();
        for along in Dir::HORIZONTAL {
            let to_edge = along.clockwise();
            let to_edge_offset = if matches!(to_edge, Dir::East | Dir::South) { r + off } else { r };
            let mut pos = origin.offset(0, y - 1, 0).relative_n(to_edge, to_edge_offset).relative_n(along, -r);
            let mut k = -r;
            while k < r + off {
                let leaves_above = run.foliage.contains(pos.above());
                if leaves_above && try_extension(run, chance, log, pos) {
                    try_extension(run, ext_chance, log, pos.below());
                }
                k += 1;
                pos = pos.relative(along);
            }
        }
    }
}

fn try_extension(run: &mut Run<'_, '_>, chance: f32, log: Pos, pos: Pos) -> bool {
    let dist = (pos.x - log.x).abs() + (pos.y - log.y).abs() + (pos.z - log.z).abs();
    if dist >= 7 {
        return false;
    }
    if run.rng.next_float() > chance { false } else { run.try_place_leaf(pos) }
}

// --- poplar: rhombus-shaped rows with a flip, plus logs replacing some leaves ---

fn is_partial_row(foliage_height: i32, y: i32) -> bool {
    foliage_height - 1 == y || foliage_height - 2 == y
}

fn corner_cut(dx: i32, dz: i32, r: i32, partial: bool, flip: bool) -> i32 {
    let lower_left_or_top_right = (dx > 0 && dz < 0) || (dz > 0 && dx < 0);
    let top_left_or_lower_right = (dx > 0 && dz > 0) || (dz < 0 && dx < 0);
    let small = if flip { top_left_or_lower_right } else { lower_left_or_top_right };
    if small {
        r - 1
    } else if partial {
        r + 1
    } else {
        r
    }
}

fn within_rhombus(r: i32, adx: i32, adz: i32, cut: i32, extra: i32) -> bool {
    adx + adz <= r * 2 - (cut + extra)
}

#[allow(clippy::too_many_arguments)]
fn poplar_row(run: &mut Run<'_, '_>, origin: Pos, r: i32, y: i32, double: bool, foliage_height: i32, flip: bool, side_hole: f32) {
    let off = i32::from(double);
    for dx in -r..=r + off {
        for dz in -r..=r + off {
            let partial = is_partial_row(foliage_height, y);
            let cut = corner_cut(dx, dz, r, partial, flip);
            let (adx, adz) = (dx.abs(), dz.abs());
            let edge = adx == r || adz == r;
            let skip = if partial && edge {
                true
            } else {
                let extra = i32::from(run.rng.next_float() <= side_hole);
                !within_rhombus(r, adx, adz, cut, extra)
            };
            if !skip {
                run.try_place_leaf(origin.offset(dx, y, dz));
            }
        }
    }
}

fn poplar(run: &mut Run<'_, '_>, a: &Attachment, offset: i32, with_offset: i32, leaf_radius: i32, side_hole: f32) {
    let double = a.double_trunk();
    let pos = a.pos.offset(0, offset, 0);
    let r = leaf_radius + a.radius_offset - 1;
    let flip = run.rng.next_bool();
    poplar_row(run, pos, r - 2, with_offset - 1, double, with_offset, flip, side_hole);
    poplar_row(run, pos, r - 1, with_offset - 2, double, with_offset, flip, side_hole);
    poplar_row(run, pos, r - 1, with_offset - 3, double, with_offset, flip, side_hole);
    for y in (1..=with_offset - 4).rev() {
        poplar_row(run, pos, r, y, double, with_offset, flip, side_hole);
    }
    replace_leaves_with_log(run, pos, r, with_offset - 4, double, with_offset, flip);
    poplar_row(run, pos, r - 1, 0, double, with_offset, flip, side_hole);
    poplar_row(run, pos, (r - 2).clamp(1, 2), -1, double, with_offset, flip, side_hole);
}

fn replace_leaves_with_log(run: &mut Run<'_, '_>, origin: Pos, r: i32, y: i32, double: bool, foliage_height: i32, flip: bool) {
    let off = i32::from(double);
    for dx in -r..=r + off {
        for dz in -r..=r + off {
            let (adx, adz) = (dx.abs(), dz.abs());
            let cut = corner_cut(dx, dz, r, is_partial_row(foliage_height, y), flip);
            if within_rhombus(r, adx, adz, cut, 2) && ((adz == 0 && r - adx >= 4) || (adx == 0 && r - adz >= 4)) {
                let p = origin.offset(dx, y, dz);
                let want = run.cfg.foliage.get(run.level, run.rng, p.x, p.y, p.z);
                if run.get(p) == want {
                    let mut log = run.cfg.trunk.get(run.level, run.rng, p.x, p.y, p.z);
                    let axis = if adz == 0 { "x" } else { "z" };
                    if let Some(t) = run.env.blocks.with(log, "axis", axis) {
                        log = t;
                    }
                    run.set_foliage(p, log);
                }
            }
        }
    }
}
