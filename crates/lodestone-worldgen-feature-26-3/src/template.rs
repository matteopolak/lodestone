//! Structure templates: the block lists the `template` and `fossil` features stamp into the world,
//! with the rotation, processor and edge-update rules of the reference placement.
//!
//! A template is a gzip-compressed binary document of a palette and a block list. Its blocks are
//! stored (and placed) in a fixed order: full-cube blocks first, then everything else, then
//! block entities, each group by y, then x, then z. Placement is two passes: processors run over
//! every block first (a rotting processor draws once per block from the feature's own random),
//! then the survivors are written; after that the shape of every cell on the outer faces of the
//! placed region is re-derived, and each placed block is recomputed from its six neighbours.

use std::sync::Arc;

use lodestone_worldgen_core::rng::RandomSource;
use serde_json::{Map, Value};

use crate::blocks::{BlockId, Dir, FluidKind, State, Support};
use crate::env::Env;
use crate::json::{Res, array, float, get, string, type_of};
use crate::level::Level;
use crate::nbt::Tag;
use crate::pos::{Pos, Rng};
use crate::shape::edge_faces;
use crate::tags::BlockSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rotation {
    None,
    Cw90,
    Cw180,
    Ccw90,
}

impl Rotation {
    /// The reference enumeration order.
    pub const ALL: [Rotation; 4] = [Rotation::None, Rotation::Cw90, Rotation::Cw180, Rotation::Ccw90];

    fn turns(self) -> usize {
        match self {
            Rotation::None => 0,
            Rotation::Cw90 => 1,
            Rotation::Cw180 => 2,
            Rotation::Ccw90 => 3,
        }
    }

    /// A horizontal direction turned by this rotation (vertical directions stay).
    #[must_use]
    pub fn rotate_dir(self, d: Dir) -> Dir {
        (0..self.turns()).fold(d, |d, _| d.clockwise())
    }

    /// A template-relative position as placed (rotation about the origin).
    #[must_use]
    pub fn transform(self, p: Pos) -> Pos {
        match self {
            Rotation::Ccw90 => Pos::new(p.z, p.y, -p.x),
            Rotation::Cw90 => Pos::new(-p.z, p.y, p.x),
            Rotation::Cw180 => Pos::new(-p.x, p.y, -p.z),
            Rotation::None => p,
        }
    }
}

#[derive(Clone, Debug)]
struct Block {
    pos: Pos,
    state: State,
    /// The block entity's type name when the template stores one.
    entity: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Template {
    size: (i32, i32, i32),
    blocks: Vec<Block>,
}

impl Template {
    /// Loads a bundled template (`fossil/skull_1`).
    ///
    /// # Errors
    /// On an unknown template or a malformed document.
    pub fn load(env: &Env, name: &str) -> Res<Self> {
        let bytes = lodestone_worldgen_data_26_3::find_bytes(lodestone_worldgen_data_26_3::STRUCTURE, name)
            .ok_or_else(|| format!("unknown structure template {name}"))?;
        let root = crate::nbt::parse_gzip(bytes)?;
        let ints = |t: Option<&Tag>| -> Vec<i32> { t.map(|t| t.as_list().iter().filter_map(Tag::as_int).collect()).unwrap_or_default() };
        let size = ints(root.get("size"));
        if size.len() != 3 {
            return Err(format!("{name}: size"));
        }
        let palette = root.get("palette").ok_or_else(|| format!("{name}: palette"))?.as_list();
        let mut states = Vec::with_capacity(palette.len());
        for entry in palette {
            states.push(env.blocks.parse_state(&tag_to_json(entry)).map_err(|e| format!("{name}: {e}"))?);
        }
        let mut full = Vec::new();
        let mut other = Vec::new();
        let mut entities = Vec::new();
        for b in root.get("blocks").ok_or_else(|| format!("{name}: blocks"))?.as_list() {
            let p = ints(b.get("pos"));
            let state = *states
                .get(b.get("state").and_then(Tag::as_int).unwrap_or(0) as usize)
                .ok_or_else(|| format!("{name}: state index"))?;
            let entity = b.get("nbt").map(|n| n.get("id").and_then(Tag::as_str).unwrap_or("").to_owned());
            let block = Block { pos: Pos::new(p[0], p[1], p[2]), state, entity };
            if block.entity.is_some() {
                entities.push(block);
            } else if env.blocks.full_collision(state) {
                full.push(block);
            } else {
                other.push(block);
            }
        }
        let key = |b: &Block| (b.pos.y, b.pos.x, b.pos.z);
        full.sort_by_key(key);
        other.sort_by_key(key);
        entities.sort_by_key(key);
        full.extend(other);
        full.extend(entities);
        Ok(Self { size: (size[0], size[1], size[2]), blocks: full })
    }

    /// The extent after `rotation` (a quarter turn swaps x and z).
    #[must_use]
    pub fn size(&self, rotation: Rotation) -> (i32, i32, i32) {
        match rotation {
            Rotation::Cw90 | Rotation::Ccw90 => (self.size.2, self.size.1, self.size.0),
            _ => self.size,
        }
    }

    #[must_use]
    pub fn raw_size(&self) -> (i32, i32, i32) {
        self.size
    }

    /// The origin to place at so a rotated template's low corner lands on `zero`.
    #[must_use]
    pub fn zero_position_with_transform(&self, zero: Pos, rotation: Rotation) -> Pos {
        let (sx, sz) = (self.size.0 - 1, self.size.2 - 1);
        match rotation {
            Rotation::Ccw90 => zero.offset(0, 0, sx),
            Rotation::Cw90 => zero.offset(sz, 0, 0),
            Rotation::Cw180 => zero.offset(sx, 0, sz),
            Rotation::None => zero,
        }
    }

    /// The eight corners of the placed bounding box.
    #[must_use]
    pub fn corners(&self, position: Pos, rotation: Rotation) -> Vec<Pos> {
        let far = Pos::new(self.size.0 - 1, self.size.1 - 1, self.size.2 - 1);
        let (a, b) = (rotation.transform(Pos::new(0, 0, 0)), rotation.transform(far));
        let (lo, hi) = (Pos::new(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)), Pos::new(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)));
        let mut out = Vec::with_capacity(8);
        for x in [lo.x, hi.x] {
            for y in [lo.y, hi.y] {
                for z in [lo.z, hi.z] {
                    out.push(position.offset(x, y, z));
                }
            }
        }
        out
    }
}

fn tag_to_json(t: &Tag) -> Value {
    match t {
        Tag::Str(s) => Value::String(s.clone()),
        Tag::Compound(c) => {
            let mut m = Map::new();
            for (k, v) in c {
                m.insert(k.clone(), tag_to_json(v));
            }
            Value::Object(m)
        }
        other => Value::String(format!("{other:?}")),
    }
}

/// A predicate over one block state.
#[derive(Clone, Debug)]
pub enum RuleTest {
    AlwaysTrue,
    BlockMatch(BlockId),
    TagMatch(BlockSet),
    StateMatch(State),
}

impl RuleTest {
    fn parse(env: &Env, v: &Value, ctx: &str, unsupported: &mut Vec<String>) -> Res<Self> {
        let kind = string(v, "predicate_type", ctx)?;
        Ok(match kind.strip_prefix("minecraft:").unwrap_or(kind) {
            "always_true" => Self::AlwaysTrue,
            "block_match" => {
                let name = string(v, "block", ctx)?;
                Self::BlockMatch(env.blocks.block_by_name(name.strip_prefix("minecraft:").unwrap_or(name)).ok_or_else(|| format!("{ctx}: unknown block {name}"))?)
            }
            "tag_match" => {
                let tag = string(v, "tag", ctx)?;
                Self::TagMatch(env.tags.get(tag).ok_or_else(|| format!("{ctx}: unknown tag {tag}"))?.clone())
            }
            "blockstate_match" => Self::StateMatch(env.blocks.parse_state(get(v, "block_state", ctx)?)?),
            other => {
                unsupported.push(format!("rule test {other}"));
                Self::AlwaysTrue
            }
        })
    }

    fn test(&self, env: &Env, s: State) -> bool {
        match self {
            Self::AlwaysTrue => true,
            Self::BlockMatch(b) => env.blocks.block_of(s) == *b,
            Self::TagMatch(t) => t.contains(env.blocks.block_of(s)),
            Self::StateMatch(m) => *m == s,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Rule {
    input: RuleTest,
    location: RuleTest,
    output: State,
}

#[derive(Clone, Debug)]
pub enum Processor {
    /// The first rule whose predicates hold replaces the block.
    Rule(Vec<Rule>),
    /// Drops blocks at random: each rottable block survives with probability `integrity`.
    BlockRot { integrity: f32, rottable: Option<Vec<BlockId>> },
    /// Drops blocks that would land on a protected world block.
    Protected(Vec<BlockId>),
}

/// A processor list (inline or by registry name).
#[derive(Clone, Debug, Default)]
pub struct Processors {
    list: Vec<Processor>,
    /// Processor types that are not ported (the feature places nothing).
    pub unsupported: Vec<String>,
}

impl Processors {
    pub fn parse(env: &Env, v: &Value, ctx: &str) -> Res<Self> {
        if let Value::String(name) = v {
            let doc = lodestone_worldgen_data_26_3::find(lodestone_worldgen_data_26_3::PROCESSOR_LIST, name)
                .ok_or_else(|| format!("{ctx}: unknown processor list {name}"))?;
            let doc: Value = serde_json::from_str(doc).map_err(|e| format!("processor_list/{name}: {e}"))?;
            return Self::parse(env, &doc, ctx);
        }
        let mut out = Self::default();
        for p in array(v, "processors", ctx)? {
            let kind = type_of(p, ctx).or_else(|_| string(p, "processor_type", ctx))?;
            let kind = kind.strip_prefix("minecraft:").unwrap_or(kind);
            match kind {
                "rule" => {
                    let mut rules = Vec::new();
                    for r in array(p, "rules", ctx)? {
                        if let Some(pos) = r.get("position_predicate") {
                            if string(pos, "predicate_type", ctx)? != "minecraft:always_true" {
                                out.unsupported.push("position predicate".into());
                            }
                        }
                        rules.push(Rule {
                            input: RuleTest::parse(env, get(r, "input_predicate", ctx)?, ctx, &mut out.unsupported)?,
                            location: RuleTest::parse(env, get(r, "location_predicate", ctx)?, ctx, &mut out.unsupported)?,
                            output: env.blocks.parse_state(get(r, "output_state", ctx)?)?,
                        });
                    }
                    out.list.push(Processor::Rule(rules));
                }
                "block_rot" => {
                    let rottable = match p.get("rottable_blocks") {
                        Some(b) => Some(crate::feature::tree::block_list(env, b, ctx)?),
                        None => None,
                    };
                    out.list.push(Processor::BlockRot { integrity: float(p, "integrity", ctx)?, rottable });
                }
                "protected_blocks" => out.list.push(Processor::Protected(crate::feature::tree::block_list(env, get(p, "value", ctx)?, ctx)?)),
                other => out.unsupported.push(format!("processor {other}")),
            }
        }
        Ok(out)
    }
}

/// Where and how a template is stamped.
#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub rotation: Rotation,
    /// Blocks outside this inclusive box are neither processed nor placed.
    pub bounds: Option<(Pos, Pos)>,
}

fn inside(b: Option<(Pos, Pos)>, p: Pos) -> bool {
    b.is_none_or(|(lo, hi)| p.x >= lo.x && p.x <= hi.x && p.y >= lo.y && p.y <= hi.y && p.z >= lo.z && p.z <= hi.z)
}

/// A state turned by a rotation: `axis`, `facing` and the four side properties follow it.
fn rotate_state(env: &Env, s: State, rotation: Rotation) -> State {
    if rotation == Rotation::None {
        return s;
    }
    let b = &env.blocks;
    let mut out = s;
    if rotation != Rotation::Cw180 {
        match b.get(s, "axis") {
            Some("x") => out = b.with(out, "axis", "z").expect("axis"),
            Some("z") => out = b.with(out, "axis", "x").expect("axis"),
            _ => {}
        }
    }
    if let Some(f) = b.get(s, "facing").and_then(Dir::from_name) {
        if f != Dir::Up && f != Dir::Down {
            out = b.with(out, "facing", rotation.rotate_dir(f).name()).expect("facing");
        }
    }
    if Dir::HORIZONTAL.iter().all(|d| b.has_property(s, d.name())) {
        for d in Dir::HORIZONTAL {
            let v = b.get(s, d.name()).expect("side").to_owned();
            out = b.with(out, rotation.rotate_dir(d).name(), &v).expect("side");
        }
    }
    out
}

fn is_speleothem(env: &Env, s: State) -> bool {
    env.tags.get("speleothems").is_some_and(|t| t.contains(env.blocks.block_of(s)))
}

fn speleothem_with_direction(level: &Level<'_>, s: State, tip: Dir) -> bool {
    is_speleothem(level.env, s) && level.env.blocks.get(s, "vertical_direction") == Some(tip.name())
}

/// What a neighbour's change does to a block, for the block kinds the bundled templates contain:
/// pointed spikes re-derive their thickness; everything else keeps its state (the remaining
/// reactions only schedule ticks, which the reference region drops).
fn update_shape(level: &Level<'_>, state: State, pos: Pos, dir: Dir) -> State {
    let env = level.env;
    let blocks = &env.blocks;
    if !is_speleothem(env, state) || !matches!(dir, Dir::Up | Dir::Down) {
        return state;
    }
    let Some(tip) = blocks.get(state, "vertical_direction").and_then(Dir::from_name) else { return state };
    let base = tip.opposite();
    let get = |p: Pos| level.get(p.x, p.y, p.z);
    if dir == base {
        let behind = get(pos.relative(base));
        let valid = blocks.face_sturdy(behind, tip, Support::Full)
            || (speleothem_with_direction(level, behind, tip) && blocks.block_of(behind) == blocks.block_of(state));
        if !valid {
            return state;
        }
    }
    let merge = blocks.get(state, "thickness") == Some("tip_merge");
    let front = get(pos.relative(tip));
    let thickness = if speleothem_with_direction(level, front, base) && blocks.block_of(front) == blocks.block_of(state) {
        if !merge && blocks.get(front, "thickness") != Some("tip_merge") { "tip" } else { "tip_merge" }
    } else if !speleothem_with_direction(level, front, tip) {
        "tip"
    } else if !matches!(blocks.get(front, "thickness"), Some("tip" | "tip_merge")) {
        if speleothem_with_direction(level, get(pos.relative(base)), tip) { "middle" } else { "base" }
    } else {
        "frustum"
    };
    blocks.with(state, "thickness", thickness).expect("thickness")
}

fn is_container(entity: &str) -> bool {
    matches!(
        entity.strip_prefix("minecraft:").unwrap_or(entity),
        "chest" | "trapped_chest" | "barrel" | "dispenser" | "dropper" | "hopper" | "furnace" | "blast_furnace" | "smoker" | "brewing_stand"
    ) || entity.contains("shulker_box")
}

/// Stamps `template` at `position`; returns whether anything was processed.
pub fn place_in_world(
    level: &mut Level<'_>,
    rng: &mut Rng,
    template: &Template,
    position: Pos,
    processors: &Processors,
    settings: Settings,
) -> bool {
    // The single palette is still chosen with a draw from the placement's random.
    rng.next_int_bounded(1);
    if template.blocks.is_empty() || template.size.0 < 1 || template.size.1 < 1 || template.size.2 < 1 {
        return false;
    }
    let env = level.env;
    // Processing pass: every block first, in template order.
    let mut processed: Vec<(Pos, State, Option<&str>)> = Vec::with_capacity(template.blocks.len());
    'blocks: for b in &template.blocks {
        let at = settings.rotation.transform(b.pos);
        let at = Pos::new(at.x + position.x, at.y + position.y, at.z + position.z);
        if !inside(settings.bounds, at) {
            continue;
        }
        let mut state = b.state;
        for p in &processors.list {
            match p {
                Processor::Rule(rules) => {
                    for r in rules {
                        if r.input.test(env, state) && r.location.test(env, level.get(at.x, at.y, at.z)) {
                            state = r.output;
                            break;
                        }
                    }
                }
                Processor::BlockRot { integrity, rottable } => {
                    let applies = rottable.as_ref().is_none_or(|t| t.contains(&env.blocks.block_of(state)));
                    if applies && !(rng.next_float() <= *integrity) {
                        continue 'blocks;
                    }
                }
                Processor::Protected(protected) => {
                    if protected.contains(&env.blocks.block_of(level.get(at.x, at.y, at.z))) {
                        continue 'blocks;
                    }
                }
            }
        }
        processed.push((at, state, b.entity.as_deref()));
    }

    // Placement pass.
    let blocks = &env.blocks;
    let mut placed: Vec<Pos> = Vec::new();
    let mut to_fill: Vec<Pos> = Vec::new();
    let mut locked: Vec<Pos> = Vec::new();
    for (at, state, entity) in processed {
        let previous = blocks.fluid(level.get(at.x, at.y, at.z));
        let previous_source = blocks.fluid_is_source(level.get(at.x, at.y, at.z));
        let state = rotate_state(env, state, settings.rotation);
        if !level.set(at.x, at.y, at.z, state) {
            continue;
        }
        placed.push(at);
        if entity.is_some_and(is_container) {
            rng.next_long();
        }
        if blocks.fluid(state) != FluidKind::Empty && blocks.fluid_is_source(state) {
            locked.push(at);
        } else if blocks.has_property(state, "waterlogged") {
            if previous == FluidKind::Water && blocks.get(state, "waterlogged") == Some("false") {
                let wet = blocks.with(state, "waterlogged", "true").expect("waterlogged");
                level.set(at.x, at.y, at.z, wet);
            }
            if !(previous != FluidKind::Empty && previous_source) {
                to_fill.push(at);
            }
        }
    }
    let mut filled = true;
    while filled && !to_fill.is_empty() {
        filled = false;
        let mut i = 0;
        while i < to_fill.len() {
            let at = to_fill[i];
            let here = level.get(at.x, at.y, at.z);
            let mut kind = blocks.fluid(here);
            let mut source = blocks.fluid_is_source(here);
            for d in [Dir::Up, Dir::North, Dir::East, Dir::South, Dir::West] {
                if source && kind != FluidKind::Empty {
                    break;
                }
                let n = at.relative(d);
                let ns = level.get(n.x, n.y, n.z);
                if blocks.fluid(ns) != FluidKind::Empty && blocks.fluid_is_source(ns) && !locked.contains(&n) {
                    kind = blocks.fluid(ns);
                    source = true;
                }
            }
            if source && kind != FluidKind::Empty {
                if blocks.has_property(here, "waterlogged") {
                    if kind == FluidKind::Water && blocks.get(here, "waterlogged") == Some("false") {
                        let wet = blocks.with(here, "waterlogged", "true").expect("waterlogged");
                        level.set(at.x, at.y, at.z, wet);
                    }
                    filled = true;
                    to_fill.remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }

    if let Some(first) = placed.first() {
        let (mut lo, mut hi) = (*first, *first);
        for p in &placed {
            lo = Pos::new(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = Pos::new(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
        }
        let (sy, sz) = ((hi.y - lo.y + 1) as usize, (hi.z - lo.z + 1) as usize);
        let mut shape = vec![false; (hi.x - lo.x + 1) as usize * sy * sz];
        for p in &placed {
            shape[((p.x - lo.x) as usize * sy + (p.y - lo.y) as usize) * sz + (p.z - lo.z) as usize] = true;
        }
        for (dir, pos) in edge_faces(lo, hi, &shape) {
            let neighbor = pos.relative(dir);
            let state = level.get(pos.x, pos.y, pos.z);
            let new = update_shape(level, state, pos, dir);
            if new != state {
                level.set(pos.x, pos.y, pos.z, new);
            }
            let ns = level.get(neighbor.x, neighbor.y, neighbor.z);
            let new_neighbor = update_shape(level, ns, neighbor, dir.opposite());
            if new_neighbor != ns {
                level.set(neighbor.x, neighbor.y, neighbor.z, new_neighbor);
            }
        }
        for p in &placed {
            let state = level.get(p.x, p.y, p.z);
            let mut new = state;
            for d in [Dir::West, Dir::East, Dir::North, Dir::South, Dir::Down, Dir::Up] {
                let probe = update_shape(level, new, *p, d);
                new = probe;
            }
            if new != state {
                level.set(p.x, p.y, p.z, new);
            }
        }
    }
    true
}

/// A template shared between placements of the same feature.
pub type SharedTemplate = Arc<Template>;
