//! The 26.3 block-state table: property domains, per-state fact words and name lookups.
//!
//! The table is parsed from `BLOCK_FACTS` (see `BlockFactsOracle263`), so every fact a feature
//! tests is the real server's own value for that state. A [`State`] is the state's global id;
//! the states of a block are the cartesian product of its properties sorted by name, last
//! property fastest, which the oracle asserts when it dumps the table.

use std::collections::HashMap;

use serde_json::Value;

/// A block-state id.
pub type State = u16;
/// A block id (index in registry order).
pub type BlockId = u16;

/// Directions in the order of the reference enumeration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dir {
    Down = 0,
    Up = 1,
    North = 2,
    South = 3,
    West = 4,
    East = 5,
}

impl Dir {
    pub const ALL: [Dir; 6] = [Dir::Down, Dir::Up, Dir::North, Dir::South, Dir::West, Dir::East];
    pub const HORIZONTAL: [Dir; 4] = [Dir::North, Dir::East, Dir::South, Dir::West];

    #[must_use]
    pub fn step(self) -> (i32, i32, i32) {
        match self {
            Dir::Down => (0, -1, 0),
            Dir::Up => (0, 1, 0),
            Dir::North => (0, 0, -1),
            Dir::South => (0, 0, 1),
            Dir::West => (-1, 0, 0),
            Dir::East => (1, 0, 0),
        }
    }

    #[must_use]
    pub fn opposite(self) -> Dir {
        match self {
            Dir::Down => Dir::Up,
            Dir::Up => Dir::Down,
            Dir::North => Dir::South,
            Dir::South => Dir::North,
            Dir::West => Dir::East,
            Dir::East => Dir::West,
        }
    }

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Dir::Down => "down",
            Dir::Up => "up",
            Dir::North => "north",
            Dir::South => "south",
            Dir::West => "west",
            Dir::East => "east",
        }
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Dir> {
        Dir::ALL.into_iter().find(|d| d.name() == name)
    }
}

/// How a face is tested for support.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Support {
    Full = 0,
    Center = 1,
    Rigid = 2,
}

/// The kind of fluid in a state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FluidKind {
    Empty,
    Water,
    Lava,
    Other,
}

const F_AIR: u64 = 1;
const F_REPLACEABLE: u64 = 1 << 1;
const F_SOLID: u64 = 1 << 2;
const F_OCCLUDE: u64 = 1 << 3;
const F_SOLID_RENDER: u64 = 1 << 4;
const F_FULL_COLLISION: u64 = 1 << 5;
const F_BLOCK_ENTITY: u64 = 1 << 41;
const F_LIQUID: u64 = 1 << 43;
const F_COLLISION_UP_FULL: u64 = 1 << 44;

#[derive(Clone, Debug)]
pub struct Property {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct BlockInfo {
    pub name: String,
    pub first: u32,
    pub default_offset: u32,
    pub count: u32,
    pub props: Vec<Property>,
}

/// All blocks and states of the release.
#[derive(Debug)]
pub struct BlockTable {
    pub blocks: Vec<BlockInfo>,
    by_name: HashMap<String, BlockId>,
    state_block: Vec<BlockId>,
    facts: Vec<u64>,
    /// Hash of each state's full key (`name[k=v,...]`), the identity the oracle hashes.
    key_hash: Vec<u64>,
}

fn fnv(s: &str) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl BlockTable {
    /// Parses the bundled table.
    ///
    /// # Panics
    /// If the bundled table is malformed, which is a build-data defect.
    #[must_use]
    pub fn load() -> Self {
        Self::parse(lodestone_worldgen_data_26_3::BLOCK_FACTS)
    }

    #[must_use]
    pub fn parse(text: &str) -> Self {
        let mut blocks: Vec<BlockInfo> = Vec::new();
        let mut facts = Vec::new();
        let mut lines = text.lines();
        while let Some(line) = lines.next() {
            let mut f = line.split(' ');
            assert_eq!(f.next(), Some("block"));
            let name = f.next().expect("name").to_owned();
            let first: u32 = f.next().expect("first").parse().expect("first id");
            let default_offset: u32 = f.next().expect("default").parse().expect("default");
            let props: Vec<Property> = f
                .map(|p| {
                    let (n, v) = p.split_once('=').expect("prop=values");
                    Property { name: n.to_owned(), values: v.split(',').map(str::to_owned).collect() }
                })
                .collect();
            let count: u32 = props.iter().map(|p| p.values.len() as u32).product();
            let fl = lines.next().expect("facts line");
            let mut got = 0u32;
            for tok in fl.split(' ').skip(1) {
                let (hex, run) = tok.split_once('*').map_or((tok, 1u32), |(h, r)| (h, r.parse().expect("run")));
                let w = u64::from_str_radix(hex, 16).expect("fact hex");
                for _ in 0..run {
                    facts.push(w);
                }
                got += run;
            }
            assert_eq!(got, count, "{name}: fact count");
            assert_eq!(first as usize + count as usize, facts.len(), "{name}: contiguous ids");
            blocks.push(BlockInfo { name, first, default_offset, count, props });
        }
        assert!(facts.len() <= State::MAX as usize, "state ids fit u16");
        let mut by_name = HashMap::new();
        let mut state_block = Vec::with_capacity(facts.len());
        for (i, b) in blocks.iter().enumerate() {
            by_name.insert(b.name.clone(), i as BlockId);
            for _ in 0..b.count {
                state_block.push(i as BlockId);
            }
        }
        let mut table = Self { blocks, by_name, state_block, facts, key_hash: Vec::new() };
        table.key_hash = (0..table.facts.len() as u32).map(|s| fnv(&table.full_key(s as State))).collect();
        table
    }

    #[must_use]
    pub fn state_count(&self) -> usize {
        self.facts.len()
    }

    #[must_use]
    pub fn block_of(&self, s: State) -> BlockId {
        self.state_block[s as usize]
    }

    #[must_use]
    pub fn block_by_name(&self, name: &str) -> Option<BlockId> {
        if name.contains(':') {
            self.by_name.get(name).copied()
        } else {
            self.by_name.get(&format!("minecraft:{name}")).copied()
        }
    }

    #[must_use]
    pub fn block_name(&self, b: BlockId) -> &str {
        &self.blocks[b as usize].name
    }

    #[must_use]
    pub fn default_state(&self, b: BlockId) -> State {
        let i = &self.blocks[b as usize];
        (i.first + i.default_offset) as State
    }

    #[must_use]
    pub fn first_state(&self, b: BlockId) -> State {
        self.blocks[b as usize].first as State
    }

    /// The key hash of a state (the identity the decoration oracle hashes).
    #[must_use]
    pub fn key_hash(&self, s: State) -> u64 {
        self.key_hash[s as usize]
    }

    /// The `name[k=v,...]` key with properties sorted by name.
    #[must_use]
    pub fn full_key(&self, s: State) -> String {
        let b = &self.blocks[self.state_block[s as usize] as usize];
        let mut out = b.name.clone();
        if !b.props.is_empty() {
            out.push('[');
            let mut rem = s as u32 - b.first;
            let mut vals = vec![0usize; b.props.len()];
            for (i, p) in b.props.iter().enumerate().rev() {
                vals[i] = (rem % p.values.len() as u32) as usize;
                rem /= p.values.len() as u32;
            }
            for (i, p) in b.props.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&p.name);
                out.push('=');
                out.push_str(&p.values[vals[i]]);
            }
            out.push(']');
        }
        out
    }

    fn fact(&self, s: State) -> u64 {
        self.facts[s as usize]
    }

    #[must_use]
    pub fn is_air(&self, s: State) -> bool {
        self.fact(s) & F_AIR != 0
    }

    #[must_use]
    pub fn replaceable(&self, s: State) -> bool {
        self.fact(s) & F_REPLACEABLE != 0
    }

    #[must_use]
    pub fn solid(&self, s: State) -> bool {
        self.fact(s) & F_SOLID != 0
    }

    #[must_use]
    pub fn can_occlude(&self, s: State) -> bool {
        self.fact(s) & F_OCCLUDE != 0
    }

    #[must_use]
    pub fn solid_render(&self, s: State) -> bool {
        self.fact(s) & F_SOLID_RENDER != 0
    }

    #[must_use]
    pub fn full_collision(&self, s: State) -> bool {
        self.fact(s) & F_FULL_COLLISION != 0
    }

    /// Whether the collision shape's up face is a full square (what a snow layer needs beneath it).
    #[must_use]
    pub fn collision_up_full(&self, s: State) -> bool {
        self.fact(s) & F_COLLISION_UP_FULL != 0
    }

    /// The block's own liquid flag (water, lava and the bubble column), distinct from carrying a fluid.
    #[must_use]
    pub fn liquid(&self, s: State) -> bool {
        self.fact(s) & F_LIQUID != 0
    }

    #[must_use]
    pub fn has_block_entity(&self, s: State) -> bool {
        self.fact(s) & F_BLOCK_ENTITY != 0
    }

    #[must_use]
    pub fn fluid(&self, s: State) -> FluidKind {
        match (self.fact(s) >> 6) & 3 {
            0 => FluidKind::Empty,
            1 => FluidKind::Water,
            2 => FluidKind::Lava,
            _ => FluidKind::Other,
        }
    }

    #[must_use]
    pub fn fluid_is_source(&self, s: State) -> bool {
        self.fact(s) & (1 << 8) != 0
    }

    #[must_use]
    pub fn fluid_amount(&self, s: State) -> u32 {
        ((self.fact(s) >> 10) & 15) as u32
    }

    #[must_use]
    pub fn face_sturdy(&self, s: State, d: Dir, t: Support) -> bool {
        self.fact(s) & (1 << (14 + (t as u32) * 6 + d as u32)) != 0
    }

    #[must_use]
    pub fn light_emission(&self, s: State) -> u32 {
        ((self.fact(s) >> 32) & 15) as u32
    }

    #[must_use]
    pub fn light_dampening(&self, s: State) -> u32 {
        ((self.fact(s) >> 36) & 15) as u32
    }

    #[must_use]
    pub fn propagates_skylight(&self, s: State) -> bool {
        self.fact(s) & (1 << 40) != 0
    }

    /// The value of a property of a state.
    #[must_use]
    pub fn get(&self, s: State, prop: &str) -> Option<&str> {
        let b = &self.blocks[self.state_block[s as usize] as usize];
        let mut rem = s as u32 - b.first;
        for p in b.props.iter().rev() {
            let n = p.values.len() as u32;
            let v = (rem % n) as usize;
            rem /= n;
            if p.name == prop {
                return Some(&p.values[v]);
            }
        }
        None
    }

    /// The state with `prop` set to `value`; `None` when the block has no such property or value.
    #[must_use]
    pub fn with(&self, s: State, prop: &str, value: &str) -> Option<State> {
        let b = &self.blocks[self.state_block[s as usize] as usize];
        let mut stride = 1u32;
        let off = s as u32 - b.first;
        for p in b.props.iter().rev() {
            let n = p.values.len() as u32;
            if p.name == prop {
                let new = p.values.iter().position(|v| v == value)? as u32;
                let old = (off / stride) % n;
                return Some((s as u32 - old * stride + new * stride) as State);
            }
            stride *= n;
        }
        None
    }

    /// The value domain of a property of a state's block.
    #[must_use]
    pub fn property_values(&self, s: State, prop: &str) -> Option<&[String]> {
        self.blocks[self.state_block[s as usize] as usize].props.iter().find(|p| p.name == prop).map(|p| p.values.as_slice())
    }

    #[must_use]
    pub fn has_property(&self, s: State, prop: &str) -> bool {
        self.blocks[self.state_block[s as usize] as usize].props.iter().any(|p| p.name == prop)
    }

    /// The state for a block with the given explicit properties; absent ones take the block's
    /// defaults. `None` for an unknown property or value.
    #[must_use]
    pub fn state_of(&self, b: BlockId, props: &[(&str, &str)]) -> Option<State> {
        let mut s = self.default_state(b);
        for (k, v) in props {
            s = self.with(s, k, v)?;
        }
        Some(s)
    }

    /// Resolves a `{ "Name": ..., "Properties": {...} }` (or `{ "id", "properties" }`) document or a bare block name.
    ///
    /// # Errors
    /// On an unknown block, property or value.
    pub fn parse_state(&self, v: &Value) -> Result<State, String> {
        match v {
            Value::String(name) => self.state_by_name(name),
            Value::Object(o) => {
                let name = o.get("Name").or_else(|| o.get("id")).and_then(Value::as_str).ok_or("state without Name")?;
                let b = self.block_by_name(name).ok_or_else(|| format!("unknown block {name}"))?;
                let mut s = self.default_state(b);
                if let Some(Value::Object(props)) = o.get("Properties").or_else(|| o.get("properties")) {
                    for (k, val) in props {
                        let val = match val {
                            Value::String(x) => x.clone(),
                            other => other.to_string(),
                        };
                        s = self.with(s, k, &val).ok_or_else(|| format!("bad property {k}={val} on {name}"))?;
                    }
                }
                Ok(s)
            }
            _ => Err("a block state is a name or an object".into()),
        }
    }

    /// Resolves `name` or `name[k=v,...]`.
    ///
    /// # Errors
    /// On an unknown block, property or value.
    pub fn state_by_name(&self, text: &str) -> Result<State, String> {
        let (name, rest) = text.split_once('[').map_or((text, ""), |(n, r)| (n, r.trim_end_matches(']')));
        let b = self.block_by_name(name).ok_or_else(|| format!("unknown block {name}"))?;
        let mut s = self.default_state(b);
        if !rest.is_empty() {
            for kv in rest.split(',') {
                let (k, val) = kv.split_once('=').ok_or("property without value")?;
                s = self.with(s, k, val).ok_or_else(|| format!("bad property {k}={val} on {name}"))?;
            }
        }
        Ok(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_loads_and_resolves_states() {
        let t = BlockTable::load();
        assert!(t.state_count() > 30_000);
        let air = t.block_by_name("air").unwrap();
        assert_eq!(t.default_state(air), 0);
        assert!(t.is_air(0));
        let grass = t.state_by_name("minecraft:grass_block").unwrap();
        assert_eq!(t.full_key(grass), "minecraft:grass_block[snowy=false]");
        let snowy = t.with(grass, "snowy", "true").unwrap();
        assert_ne!(grass, snowy);
        assert_eq!(t.get(snowy, "snowy"), Some("true"));
        let stone = t.state_by_name("stone").unwrap();
        assert!(t.solid(stone) && t.can_occlude(stone) && !t.is_air(stone));
        assert!(t.face_sturdy(stone, Dir::Up, Support::Full));
        let water = t.state_by_name("water[level=0]").unwrap();
        assert_eq!(t.fluid(water), FluidKind::Water);
        assert!(t.fluid_is_source(water));
        // Control: a wrong property must not resolve rather than fall back to the default.
        assert!(t.state_by_name("stone[bogus=1]").is_err());
        assert!(t.state_by_name("oak_log[axis=w]").is_err());
        let log = t.state_by_name("oak_log[axis=x]").unwrap();
        assert_eq!(t.get(log, "axis"), Some("x"));
    }
}
