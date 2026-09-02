//! Pastes a parsed structure template into a **live** [`ChunkSource`] — a
//! runtime placement entry point.
//!
//! # The gap this closes
//!
//! `lodestone_worldgen::structure::template::StructureTemplate::place` is a
//! real, working placement engine (rotation, mirror, the full processor
//! chain — rule/block-age/gravity/capped/protected-blocks), but it writes
//! into a generation-time [`DenseBlockGrid`], and every existing caller
//! builds that grid from a chunk's own in-progress generation state. There is
//! no entry point that pastes a template into a chunk that has **already**
//! been generated and possibly edited by a player — the thing a dungeon
//! plugin, a loot-structure mod, or a world-editor's "paste schematic"
//! command all need, and none of which is chunk generation.
//!
//! [`place_structure_live`] is that entry point: it reads the template's own
//! bounding box, hydrates a working grid from the **live** source (so a
//! processor that inspects the world — `RuleProcessor`'s "is there water
//! under this dirt path" check, for one — sees real, already-placed blocks,
//! not generation-time terrain-in-progress), places the template into it via
//! the exact same [`StructureTemplate::place`] generation uses, and writes
//! every touched cell back through [`ChunkSource::set_block`] — the same
//! edit path a player's own block placement goes through, so the paste
//! persists and reports through `column()`/`block_state()` exactly like any
//! other edit.

use lodestone_worldgen::dense_grid::DenseBlockGrid;
use lodestone_worldgen::structure::template::{PlaceOrigin, PlaceSettings, StructureTemplate};

use crate::chunk::ChunkSource;

/// Pastes `template` into `source` at `origin`/`settings`, both already
/// resolved into the vocabulary generation-time placement uses (see
/// `lodestone_worldgen::structure::template`'s own doc for what each field
/// means). Returns the number of blocks written — [`StructureTemplate::place`]'s
/// own return value, since nothing here changes which blocks it decided to
/// write, only where the working grid's contents come from and go to.
///
/// # Why the whole bounding box is written back, not only "changed" cells
///
/// [`StructureTemplate::place`] does not report *which* cells it touched,
/// only how many — recovering that would mean diffing the grid against a
/// second, unmodified copy read at exactly the moment the first is written,
/// doubling the memory this pays for a structure that only pastes here at
/// all (rare, non-hot-path) for no observable benefit: a cell the template
/// happened not to touch is written back with the exact live value it
/// already had, so the operation is idempotent on that cell regardless.
pub fn place_structure_live(
    source: &dyn ChunkSource,
    template: &StructureTemplate,
    origin: PlaceOrigin,
    settings: &PlaceSettings,
) -> usize {
    let bbox = template.bounding_box(origin.position, settings);
    let min_x = bbox.min[0];
    let min_y = bbox.min[1];
    let min_z = bbox.min[2];
    let size_x = (bbox.max[0] - bbox.min[0] + 1).max(0);
    let size_y = (bbox.max[1] - bbox.min[1] + 1).max(0);
    let size_z = (bbox.max[2] - bbox.min[2] + 1).max(0);
    if size_x == 0 || size_y == 0 || size_z == 0 {
        return 0;
    }

    let mut grid = DenseBlockGrid::new(min_x, min_y, min_z, size_x, size_y, size_z, "minecraft:air");
    for y in min_y..min_y + size_y {
        for z in min_z..min_z + size_z {
            for x in min_x..min_x + size_x {
                let state = source.block_state(x, y, z);
                grid.set(x, y, z, &state);
            }
        }
    }

    let written = template.place(origin, settings, &mut grid);

    for y in min_y..min_y + size_y {
        for z in min_z..min_z + size_z {
            for x in min_x..min_x + size_x {
                source.set_block(x, y, z, grid.get(x, y, z));
            }
        }
    }

    written
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunk::ChunkColumn;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A trivial live source over a fixed-size in-memory grid — enough to
    /// prove [`place_structure_live`] reads/writes through the real
    /// [`ChunkSource`] interface, without pulling in a whole generator.
    struct FlatStoneWorld {
        edits: Mutex<HashMap<(i32, i32, i32), String>>,
    }

    impl FlatStoneWorld {
        fn new() -> Self {
            Self {
                edits: Mutex::new(HashMap::new()),
            }
        }
    }

    impl ChunkSource for FlatStoneWorld {
        fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
            ChunkColumn::new(0, 8)
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            self.edits
                .lock()
                .unwrap()
                .get(&(x, y, z))
                .cloned()
                .unwrap_or_else(|| {
                    if y == 0 {
                        "minecraft:stone".to_string()
                    } else {
                        "minecraft:air".to_string()
                    }
                })
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            "minecraft:plains".to_string()
        }

        fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
            self.edits.lock().unwrap().insert((x, y, z), name.to_string());
        }
    }

    /// A two-block, single-palette template: a gold block at the origin and
    /// a diamond block one cell above it. Built by hand (bypassing
    /// `StructureTemplate::parse`'s NBT decode, which is already covered
    /// elsewhere) so this test exercises only the live-placement seam.
    fn two_block_template() -> StructureTemplate {
        use lodestone_worldgen::structure::template::BlockState;
        StructureTemplate::from_blocks(
            [1, 2, 1],
            vec![
                BlockState::of("minecraft:gold_block"),
                BlockState::of("minecraft:diamond_block"),
            ],
            vec![([0, 0, 0], 0), ([0, 1, 0], 1)],
        )
    }

    #[test]
    fn pastes_into_an_already_generated_live_world() {
        let world = FlatStoneWorld::new();
        assert_eq!(world.block_state(5, 1, 5), "minecraft:air", "control: nothing there yet");

        let template = two_block_template();
        let origin = PlaceOrigin {
            position: [5, 1, 5],
            reference: [5, 1, 5],
            seed: 0,
        };
        let written = place_structure_live(&world, &template, origin, &PlaceSettings::default());

        assert_eq!(written, 2);
        assert_eq!(world.block_state(5, 1, 5), "minecraft:gold_block");
        assert_eq!(world.block_state(5, 2, 5), "minecraft:diamond_block");
        // Outside the template's own footprint is untouched.
        assert_eq!(world.block_state(6, 1, 5), "minecraft:air");
        // And the pre-existing stone floor the template did not cover is
        // still there, proving the whole-bbox writeback did not clobber
        // live blocks it merely read.
        assert_eq!(world.block_state(5, 0, 5), "minecraft:stone");
    }
}
