//! A reference plugin for the worldgen plugin seams — issues #132 (a `dyn
//! ChunkGenerator` a plugin implements), #134 (registering a custom
//! dimension backed by it) and #136 (placing a structure template, both at
//! generation time and into an already-generated live world).
//!
//! [`CheckerboardVoidGenerator`] is deliberately simple — the honest way to
//! prove the seam is a demo generator a reader can check by eye, not a
//! second verified pipeline. It produces a flat glass/stone checkerboard
//! floor (vanilla's own creative "superflat" aesthetic, generalised to two
//! alternating materials) and, at chunk `(0, 0)`, places a small hand-built
//! "landmark" structure at generation time via
//! `lodestone_worldgen::structure::template::StructureTemplate::place` —
//! the same primitive every vanilla structure in this engine places with.
//! [`place_marker_live`] is the separate, issue-#136 half: pasting a second
//! template into a world that has **already** been generated, through
//! `lodestone_server::structure_placement::place_structure_live`.
//!
//! See `docs/plugin-worldgen-api.md` for the design this plugin is the
//! reference implementation of, and
//! `tests/drives_a_real_dimension_through_a_joined_client.rs` for the
//! end-to-end proof: a real `IntegratedServer` plus a real, wire-decoding
//! `lodestone-client` observe this generator's terrain and both structure
//! placements — not a test calling this crate's own functions directly.

use std::sync::Arc;

use lodestone_server::ChunkSource;
use lodestone_server::plugin_dimension::{DimensionProperties, DimensionRegistry, PluginDimension};
use lodestone_server::structure_placement::place_structure_live;
use lodestone_worldgen::dense_grid::DenseBlockGrid;
use lodestone_worldgen::generator::ChunkGenerator;
use lodestone_worldgen::structure::template::{BlockState, PlaceOrigin, PlaceSettings, StructureTemplate};

/// The key this plugin registers its dimension under
/// (see `docs/plugin-worldgen-api.md`'s naming convention: a plugin key is
/// never `minecraft:`-prefixed, so it can never shadow a hosted dimension).
pub const DIMENSION_KEY: &str = "voidworld:checkerboard";

/// World Y of the checkerboard floor and the landmark structure's base.
pub const FLOOR_Y: i32 = 0;

/// A checkerboard void world: a glass/stone floor at [`FLOOR_Y`] and nothing
/// else, plus a fixed 3×1×3 gold-and-beacon "landmark" placed once, at
/// chunk `(0, 0)`, at generation time.
#[derive(Debug, Default)]
pub struct CheckerboardVoidGenerator;

impl CheckerboardVoidGenerator {
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// The landmark template placed at chunk `(0, 0)`'s generation time: a 3×3
/// gold-block platform with a beacon in the centre, one row above the
/// checkerboard floor — small and visually distinctive on purpose, so an
/// end-to-end gate (or a person looking at the world) can tell it apart
/// from the floor pattern at a glance.
fn landmark_template() -> StructureTemplate {
    let gold = BlockState::of("minecraft:gold_block");
    let beacon = BlockState::of("minecraft:beacon");
    let mut blocks = Vec::new();
    for dx in 0..3i32 {
        for dz in 0..3i32 {
            blocks.push(([dx, 0, dz], 0u16));
        }
    }
    blocks.push(([1, 1, 1], 1u16));
    StructureTemplate::from_blocks([3, 2, 3], vec![gold, beacon], blocks)
}

impl ChunkGenerator for CheckerboardVoidGenerator {
    fn min_y(&self) -> i32 {
        -64
    }

    fn height(&self) -> i32 {
        128
    }

    fn generate(&self, cx: i32, cz: i32) -> DenseBlockGrid {
        let mut grid = DenseBlockGrid::new(cx * 16, self.min_y(), cz * 16, 16, self.height(), 16, "minecraft:air");
        for lx in 0..16i32 {
            for lz in 0..16i32 {
                let x = cx * 16 + lx;
                let z = cz * 16 + lz;
                let state = if (x + z).rem_euclid(2) == 0 {
                    "minecraft:stone"
                } else {
                    "minecraft:glass"
                };
                grid.set(x, FLOOR_Y, z, state);
            }
        }

        // Generation-time structure placement (issue #136's first half):
        // the same `StructureTemplate::place` every vanilla structure in
        // this engine uses, called directly from a plugin generator rather
        // than from `lodestone-worldgen`'s own structure-composition stage.
        if cx == 0 && cz == 0 {
            let template = landmark_template();
            let origin = PlaceOrigin {
                position: [0, FLOOR_Y + 1, 0],
                reference: [0, FLOOR_Y + 1, 0],
                seed: 0,
            };
            template.place(origin, &PlaceSettings::default(), &mut grid);
        }

        grid
    }

    fn biome(&self) -> &str {
        "minecraft:the_void"
    }
}

/// Registers [`CheckerboardVoidGenerator`] under [`DIMENSION_KEY`] into
/// `registry`.
///
/// The properties' vertical bounds are read straight off the generator
/// rather than duplicated as separate literals — `DimensionProperties` and
/// `ChunkGenerator` have no compile-time link (see
/// `docs/plugin-worldgen-api.md`'s gotcha on this), so the only way to keep
/// them from drifting apart is to derive one from the other at the one call
/// site that has both.
pub fn register(registry: &DimensionRegistry) {
    let generator = Arc::new(CheckerboardVoidGenerator::new());
    registry.register(PluginDimension {
        key: DIMENSION_KEY.to_string(),
        properties: DimensionProperties {
            min_y: generator.min_y(),
            height: generator.height(),
            logical_height: generator.height(),
            has_ceiling: false,
            has_skylight: true,
            natural: true,
            ..DimensionProperties::default()
        },
        generator,
    });
}

/// Issue #136's other half: pastes a single emerald-block marker into an
/// **already-generated** world at `at` — a dungeon plugin dropping a
/// landmark into a live save, not a generation-time decoration.
///
/// Returns the number of blocks written (always 1 for this fixed template,
/// unless `at` sits fully outside every dimension the source can represent —
/// see [`place_structure_live`]'s own doc for when that is 0).
pub fn place_marker_live(source: &dyn ChunkSource, at: [i32; 3]) -> usize {
    let template = StructureTemplate::from_blocks(
        [1, 1, 1],
        vec![BlockState::of("minecraft:emerald_block")],
        vec![([0, 0, 0], 0)],
    );
    let origin = PlaceOrigin {
        position: at,
        reference: at,
        seed: 0,
    };
    place_structure_live(source, &template, origin, &PlaceSettings::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_makes_the_dimension_reachable_by_key() {
        let registry = DimensionRegistry::new();
        assert!(registry.get(DIMENSION_KEY).is_none(), "control: nothing registered yet");
        register(&registry);
        let entry = registry.get(DIMENSION_KEY).expect("register() must have inserted it");
        assert_eq!(entry.properties.min_y, -64);
        assert_eq!(entry.properties.height, 128);
    }

    #[test]
    fn generator_produces_the_checkerboard_floor_and_the_landmark() {
        let generator = CheckerboardVoidGenerator::new();
        let grid = generator.generate(0, 0);
        assert_eq!(grid.get(0, FLOOR_Y, 0), "minecraft:stone");
        assert_eq!(grid.get(1, FLOOR_Y, 0), "minecraft:glass");
        assert_eq!(grid.get(0, FLOOR_Y, 1), "minecraft:glass");
        // The landmark: a gold platform at `FLOOR_Y + 1`, and its beacon one
        // row above that, at `FLOOR_Y + 2` — the platform's own local y=0
        // sits at world `FLOOR_Y + 1` (the template's origin), and the
        // beacon's local y=1 is one above it.
        assert_eq!(grid.get(0, FLOOR_Y + 1, 0), "minecraft:gold_block");
        assert_eq!(grid.get(1, FLOOR_Y + 2, 1), "minecraft:beacon");
        // A neighbouring chunk gets the floor but not the landmark.
        let neighbour = generator.generate(1, 0);
        assert_eq!(neighbour.get(16, FLOOR_Y + 1, 0), "minecraft:air");
    }
}
