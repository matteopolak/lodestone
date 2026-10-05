//! The shared, immutable facts decoration runs against: the block table, tags and the states
//! the engine names directly.

use crate::blocks::{BlockTable, State};
use crate::tags::BlockTags;

/// The six heightmaps, in the reference enumeration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Heightmap {
    WorldSurfaceWg = 0,
    WorldSurface = 1,
    OceanFloorWg = 2,
    OceanFloor = 3,
    MotionBlocking = 4,
    MotionBlockingNoLeaves = 5,
}

impl Heightmap {
    pub const ALL: [Heightmap; 6] = [
        Heightmap::WorldSurfaceWg,
        Heightmap::WorldSurface,
        Heightmap::OceanFloorWg,
        Heightmap::OceanFloor,
        Heightmap::MotionBlocking,
        Heightmap::MotionBlockingNoLeaves,
    ];

    /// Whether writes after terrain generation keep this heightmap current (the world-gen
    /// variants are frozen once terrain is done).
    #[must_use]
    pub fn live(self) -> bool {
        matches!(self, Heightmap::WorldSurface | Heightmap::OceanFloor | Heightmap::MotionBlocking | Heightmap::MotionBlockingNoLeaves)
    }

    #[must_use]
    pub fn from_name(name: &str) -> Option<Heightmap> {
        Some(match name {
            "WORLD_SURFACE_WG" => Heightmap::WorldSurfaceWg,
            "WORLD_SURFACE" => Heightmap::WorldSurface,
            "OCEAN_FLOOR_WG" => Heightmap::OceanFloorWg,
            "OCEAN_FLOOR" => Heightmap::OceanFloor,
            "MOTION_BLOCKING" => Heightmap::MotionBlocking,
            "MOTION_BLOCKING_NO_LEAVES" => Heightmap::MotionBlockingNoLeaves,
            _ => return None,
        })
    }
}

/// Blocks and states the engine refers to by name.
#[derive(Clone, Copy, Debug)]
pub struct Known {
    pub air: State,
    pub void_air: State,
    pub water: State,
    pub lava: State,
    pub bedrock: State,
    pub cave_air: State,
    pub ice: State,
    /// A single snow layer.
    pub snow: State,
}

/// Block table, tags and per-state heightmap membership.
#[derive(Debug)]
pub struct Env {
    pub blocks: BlockTable,
    pub tags: BlockTags,
    pub known: Known,
    /// Per state, bit `i` set when the state counts for heightmap `i`.
    hm_flags: Vec<u8>,
}

impl Env {
    /// Builds the environment from the bundled data.
    ///
    /// # Panics
    /// If the bundled data lacks a block the engine needs, which is a data defect.
    #[must_use]
    pub fn load() -> Self {
        let blocks = BlockTable::load();
        let tags = BlockTags::load(&blocks);
        let st = |n: &str| blocks.state_by_name(n).unwrap_or_else(|e| panic!("{n}: {e}"));
        let known = Known {
            air: st("minecraft:air"),
            void_air: st("minecraft:void_air"),
            water: st("minecraft:water[level=0]"),
            lava: st("minecraft:lava[level=0]"),
            bedrock: st("minecraft:bedrock"),
            cave_air: st("minecraft:cave_air"),
            ice: st("minecraft:ice"),
            snow: st("minecraft:snow"),
        };
        let motion = tags.get("blocks_motion_in_heightmap").expect("tag");
        let no_leaves = tags.get("blocks_motion_in_heightmap_no_leaves").expect("tag");
        let hm_flags = (0..blocks.state_count() as u32)
            .map(|s| {
                let s = s as State;
                let b = blocks.block_of(s);
                let fluid = blocks.fluid(s) != crate::blocks::FluidKind::Empty;
                let mut f = 0u8;
                if !blocks.is_air(s) {
                    f |= 1 << Heightmap::WorldSurfaceWg as u8 | 1 << Heightmap::WorldSurface as u8;
                }
                if motion.contains(b) {
                    f |= 1 << Heightmap::OceanFloorWg as u8 | 1 << Heightmap::OceanFloor as u8;
                }
                if motion.contains(b) || fluid {
                    f |= 1 << Heightmap::MotionBlocking as u8;
                }
                if no_leaves.contains(b) || fluid {
                    f |= 1 << Heightmap::MotionBlockingNoLeaves as u8;
                }
                f
            })
            .collect();
        Self { blocks, tags, known, hm_flags }
    }

    /// Whether `s` counts for heightmap `h`.
    #[must_use]
    pub fn counts_for(&self, h: Heightmap, s: State) -> bool {
        self.hm_flags[s as usize] & (1 << h as u8) != 0
    }

    /// Block-tag membership of a state's block.
    #[must_use]
    pub fn is_in(&self, s: State, tag: &crate::tags::BlockSet) -> bool {
        tag.contains(self.blocks.block_of(s))
    }
}
