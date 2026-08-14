//! Debug-world generation — issue #519's fourth missing generator
//! (`debug_all_block_states`), after `WorldType::{Amplified,LargeBiomes}` and
//! [`crate::flat`] landed.
//!
//! Ports `DebugLevelSource` (`net.minecraft.world.level.levelgen
//! .DebugLevelSource`): every block state in the registry laid out on a flat
//! grid, one state per odd `(x, z)` cell, at a fixed Y, with a barrier floor
//! two rows below it. It has no seed, no noise router and — like
//! [`crate::flat`] — no [`crate::density::Resolver`]: the whole layout is a
//! pure function of the ordered state list a caller supplies (this crate is
//! version-free and holds no block registry of its own, so it cannot
//! enumerate "every block state" itself — see [`DebugLevelSource::new`]).
//!
//! # The layout, transcribed from `DebugLevelSource.java`
//!
//! * `BARRIER_HEIGHT` (`60`): every `(x, z)` in a generated chunk gets a
//!   `minecraft:barrier` at this Y — a floor a player cannot dig through, so
//!   the state grid above it is always reachable by falling onto it.
//! * `HEIGHT` (`70`): [`DebugLevelSource::state_for`] decides, per world
//!   `(x, z)`, whether this Y is one state from the ordered list or air.
//! * Everywhere else is air — `fillFromNoise` is a no-op and
//!   `getBaseColumn` returns an empty column, so nothing but
//!   `applyBiomeDecoration`'s two rows ever writes a block.
//! * The biome is fixed to `minecraft:plains` everywhere
//!   (`new FixedBiomeSource(plains)`) — [`DEBUG_BIOME`].
//!
//! `getBlockStateFor`'s index math (`Mth.abs(worldX * GRID_WIDTH + worldZ)`
//! after halving both coordinates) is transcribed in [`DebugLevelSource::state_for`]
//! verbatim, including the `Mth.abs` call — it is a no-op under the guard
//! that reaches it (`worldX > 0 && worldZ > 0` before halving keeps both
//! halves non-negative), but a divergent transcription is exactly the trap
//! CLAUDE.md's evidence-standards section names, so the port keeps it rather
//! than "simplifying" the formula.

/// World Y of the `minecraft:barrier` floor under the state grid —
/// `DebugLevelSource.BARRIER_HEIGHT`.
pub const BARRIER_Y: i32 = 60;

/// World Y of the block-state grid itself — `DebugLevelSource.HEIGHT`.
pub const GRID_Y: i32 = 70;

/// The fixed biome every column reports — `DebugLevelSource`'s constructor
/// wraps a `FixedBiomeSource` around `Biomes.PLAINS`.
pub const DEBUG_BIOME: &str = "minecraft:plains";

/// One generated 16×16 column of a debug world: a barrier row at
/// [`BARRIER_Y`], the state-grid row at [`GRID_Y`] (precomputed per `(x, z)`
/// at construction, since unlike [`crate::flat::FlatColumn`] it is not
/// uniform across the column), and air everywhere else.
#[derive(Debug, Clone)]
pub struct DebugColumn {
    min_y: i32,
    height: i32,
    /// The 16×16 [`GRID_Y`] row, row-major `local_x * 16 + local_z` — see
    /// [`DebugLevelSource::column`].
    grid_row: [String; 256],
}

impl DebugColumn {
    /// World Y of the lowest row this dimension covers.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// The dimension's full column height.
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// The fixed biome — [`DEBUG_BIOME`] at every column.
    #[must_use]
    pub fn biome(&self) -> &str {
        DEBUG_BIOME
    }

    /// Canonical state at local `(local_x, y, local_z)` (`0..16` each of
    /// `local_x`/`local_z`).
    ///
    /// # Panics
    /// Panics if `local_x`/`local_z` are outside `0..16`.
    #[must_use]
    pub fn block_state(&self, local_x: i32, y: i32, local_z: i32) -> &str {
        assert!((0..16).contains(&local_x), "local_x {local_x} out of range");
        assert!((0..16).contains(&local_z), "local_z {local_z} out of range");
        if y == BARRIER_Y {
            return "minecraft:barrier";
        }
        if y == GRID_Y {
            return &self.grid_row[(local_x * 16 + local_z) as usize];
        }
        "minecraft:air"
    }

    /// Highest world Y whose block is not air, or `min_y - 1` for a column
    /// with no grid cell placed at all (mirrors
    /// [`crate::flat::FlatColumn::top_non_air_y`]'s contract).
    #[must_use]
    pub fn top_non_air_y(&self) -> i32 {
        if self.grid_row.iter().any(|s| s != "minecraft:air") {
            GRID_Y
        } else {
            BARRIER_Y
        }
    }
}

/// The debug-world generator — `DebugLevelSource`
/// (`net.minecraft.world.level.levelgen.DebugLevelSource`). See the module
/// doc for the layout.
#[derive(Debug, Clone)]
pub struct DebugLevelSource {
    /// Every block state, in the vanilla global-palette order (registry
    /// order, then per-block state-permutation order) — `ALL_BLOCKS`. This
    /// crate has no block registry, so a caller supplies the ordered list;
    /// `lodestone_server::worldgen_data::debug_generator` builds it from
    /// `lodestone_data::block_states`, whose ids are documented as that same
    /// wire/global-palette order.
    states: std::sync::Arc<[String]>,
    /// `DebugLevelSource.GRID_WIDTH` — `ceil(sqrt(states.len()))`.
    grid_width: i32,
    /// `DebugLevelSource.GRID_HEIGHT` — `ceil(states.len() / grid_width)`.
    grid_height: i32,
    min_y: i32,
    height: i32,
}

impl DebugLevelSource {
    /// Builds the generator from the full ordered state list. `min_y`/
    /// `height` are the dimension's vertical bounds (e.g. -64/384 for the
    /// overworld) — see [`crate::flat::FlatLevelSource::new`]'s doc for why
    /// this crate takes them as a parameter rather than the vanilla
    /// `getMinY`/`getGenDepth` constants (0/384), which are an unrelated
    /// `ChunkGenerator` abstract-method answer, not where blocks are placed.
    ///
    /// # Panics
    /// Panics if `states` is empty — `GRID_WIDTH`/`GRID_HEIGHT` are
    /// undefined for zero states, and no real block registry is ever empty.
    #[must_use]
    pub fn new(states: Vec<String>, min_y: i32, height: i32) -> Self {
        assert!(!states.is_empty(), "debug world needs a non-empty state list");
        let n = states.len() as f64;
        // `Mth.ceil(Mth.sqrt(n))` / `Mth.ceil((float) n / GRID_WIDTH)`.
        let grid_width = n.sqrt().ceil() as i32;
        let grid_height = (n / f64::from(grid_width)).ceil() as i32;
        Self {
            states: states.into(),
            grid_width,
            grid_height,
            min_y,
            height,
        }
    }

    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// `GRID_WIDTH` — exposed for a caller that wants to predict a placement
    /// without constructing a whole column.
    #[must_use]
    pub fn grid_width(&self) -> i32 {
        self.grid_width
    }

    /// `GRID_HEIGHT` — see [`Self::grid_width`].
    #[must_use]
    pub fn grid_height(&self) -> i32 {
        self.grid_height
    }

    /// The state placed at world `(world_x, world_z)` on the [`GRID_Y`] row —
    /// `DebugLevelSource.getBlockStateFor`, transcribed verbatim (see the
    /// module doc on the `Mth.abs` call).
    #[must_use]
    pub fn state_for(&self, world_x: i32, world_z: i32) -> &str {
        if world_x > 0 && world_z > 0 && world_x % 2 != 0 && world_z % 2 != 0 {
            let gx = world_x / 2;
            let gz = world_z / 2;
            if gx <= self.grid_width && gz <= self.grid_height {
                let index = (gx * self.grid_width + gz).unsigned_abs() as usize;
                if let Some(state) = self.states.get(index) {
                    return state;
                }
            }
        }
        "minecraft:air"
    }

    /// Generates the column at chunk coordinates `(cx, cz)`.
    #[must_use]
    pub fn column(&self, cx: i32, cz: i32) -> DebugColumn {
        let base_x = cx * 16;
        let base_z = cz * 16;
        let grid_row = std::array::from_fn(|i| {
            let local_x = (i / 16) as i32;
            let local_z = (i % 16) as i32;
            self.state_for(base_x + local_x, base_z + local_z).to_string()
        });
        DebugColumn {
            min_y: self.min_y,
            height: self.height,
            grid_row,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small hand-computable state list, so `GRID_WIDTH`/`GRID_HEIGHT` and
    /// the index formula can be predicted by hand rather than trusted.
    fn five_states() -> Vec<String> {
        vec![
            "minecraft:state_0".to_string(),
            "minecraft:state_1".to_string(),
            "minecraft:state_2".to_string(),
            "minecraft:state_3".to_string(),
            "minecraft:state_4".to_string(),
        ]
    }

    #[test]
    fn grid_dimensions_match_the_vanilla_formula() {
        // ceil(sqrt(5)) = ceil(2.236) = 3; ceil(5 / 3) = ceil(1.667) = 2.
        let generator = DebugLevelSource::new(five_states(), -64, 384);
        assert_eq!(generator.grid_width(), 3);
        assert_eq!(generator.grid_height(), 2);
    }

    /// Realistic size check against the real state count this module's doc
    /// names — 32,366 states, `ceil(sqrt(32366)) = 180`,
    /// `ceil(32366 / 180) = 180`. Derived here, not asserted from memory.
    #[test]
    fn grid_dimensions_at_real_state_count() {
        let states: Vec<String> = (0..32366).map(|i| format!("minecraft:s{i}")).collect();
        let generator = DebugLevelSource::new(states, -64, 384);
        let n = 32366_f64;
        let expected_width = n.sqrt().ceil() as i32;
        let expected_height = (n / f64::from(expected_width)).ceil() as i32;
        assert_eq!(generator.grid_width(), expected_width);
        assert_eq!(generator.grid_height(), expected_height);
        assert_eq!(generator.grid_width(), 180);
        assert_eq!(generator.grid_height(), 180);
    }

    /// The discriminating cells for the 5-state fixture: `(1, 1)` halves to
    /// `(0, 0)`, index `0*3+0=0` -> `state_0`. `(3, 1)` halves to `(1, 0)`,
    /// index `1*3+0=3` -> `state_3`. `(1, 3)` halves to `(0, 1)`, index
    /// `0*3+1=1` -> `state_1`. An even coordinate must always be air, and an
    /// index past the state list (e.g. `(5, 5)` halves to `(2, 2)`, index
    /// `2*3+2=8 >= 5`) must also be air.
    #[test]
    fn state_for_matches_hand_computed_indices() {
        let generator = DebugLevelSource::new(five_states(), -64, 384);
        let cases: Vec<((i32, i32), &str)> = vec![
            ((1, 1), "minecraft:state_0"),
            ((3, 1), "minecraft:state_3"),
            ((1, 3), "minecraft:state_1"),
            ((0, 1), "minecraft:air"),  // world_x == 0 fails world_x > 0
            ((2, 1), "minecraft:air"),  // even world_x
            ((1, 2), "minecraft:air"),  // even world_z
            ((-1, 1), "minecraft:air"), // world_x not > 0
            ((5, 5), "minecraft:air"),  // index 8 >= 5 states
        ];
        let mismatches: Vec<String> = cases
            .iter()
            .filter_map(|&((x, z), want)| {
                let got = generator.state_for(x, z);
                (got != want).then(|| format!("({x},{z}): expected {want:?}, got {got:?}"))
            })
            .collect();
        assert!(mismatches.is_empty(), "state_for mismatches: {mismatches:#?}");
    }

    #[test]
    fn column_places_barrier_and_grid_row_and_nothing_else() {
        let generator = DebugLevelSource::new(five_states(), -64, 384);
        let column = generator.column(0, 0);
        assert_eq!(column.biome(), "minecraft:plains");

        // Barrier at every (local_x, local_z) at y=BARRIER_Y.
        let mut mismatches = Vec::new();
        for lx in 0..16 {
            for lz in 0..16 {
                let got = column.block_state(lx, BARRIER_Y, lz);
                if got != "minecraft:barrier" {
                    mismatches.push(format!("barrier row ({lx},{lz}): got {got:?}"));
                }
            }
        }
        // World (1,1) -> local (1,1) at GRID_Y is state_0; local (0,0) is air
        // (world (0,0) fails world_x>0).
        if column.block_state(1, GRID_Y, 1) != "minecraft:state_0" {
            mismatches.push(format!(
                "grid (1,1): expected state_0, got {:?}",
                column.block_state(1, GRID_Y, 1)
            ));
        }
        if column.block_state(0, GRID_Y, 0) != "minecraft:air" {
            mismatches.push(format!(
                "grid (0,0): expected air, got {:?}",
                column.block_state(0, GRID_Y, 0)
            ));
        }
        // Every other Y is air.
        for &y in &[-64, 0, 59, 61, 69, 71, 100, 319] {
            let got = column.block_state(1, y, 1);
            if got != "minecraft:air" {
                mismatches.push(format!("y={y} (1,1): expected air, got {got:?}"));
            }
        }
        assert!(mismatches.is_empty(), "column mismatches: {mismatches:#?}");
        assert_eq!(column.top_non_air_y(), GRID_Y);
    }

    /// A chunk whose whole 16×16 grid-row window lands on even coordinates
    /// only (impossible for a 16-wide window since x always spans both
    /// parities, but the *state* placement itself can still be all-air if
    /// every odd-coordinate index in the window falls past the state list) —
    /// `top_non_air_y` must then report the barrier row, not the grid row.
    #[test]
    fn top_non_air_y_falls_back_to_barrier_when_grid_row_is_all_air() {
        // A single-state list makes almost every grid cell in a far-away
        // chunk resolve past the list (index >= 1), except possibly one.
        // Pick a chunk whose base coordinates guarantee every odd (x,z) in
        // the 16x16 window maps to a halved (gx,gz) exceeding grid bounds.
        let generator = DebugLevelSource::new(vec!["minecraft:only_state".to_string()], -64, 384);
        // grid_width = grid_height = 1 (ceil(sqrt(1))=1). Any (gx,gz) besides
        // (0,0) or with product+sum >=1 index gives air. Chunk far from
        // origin, e.g. (100, 100): base (1600,1600), all coordinates >> the
        // grid bound of 1, so every candidate index is out of range.
        let column = generator.column(100, 100);
        assert_eq!(column.top_non_air_y(), BARRIER_Y);
    }
}
