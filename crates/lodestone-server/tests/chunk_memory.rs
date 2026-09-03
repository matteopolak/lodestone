//! The identity gate for bit-packing: packing `ChunkColumn`'s block grid per section
//! (`src/chunk_blocks.rs`) must change the **representation** and nothing else.
//!
//! # Why an identity gate and not a memory gate
//!
//! The memory win is arithmetic once the representation is known, and this repo's
//! rules say a residency claim needs `/usr/bin/time -l`, which a `cargo test`
//! assertion cannot be. So the *saving* is asserted as an exact **byte count**
//! (`ChunkColumn::blocks_heap_bytes`, immune to machine load), and the risk —
//! serving the wrong block — gets the whole-column comparison below.
//!
//! # Where the expected values come from, and why that matters
//!
//! `decode(encode(x)) == x` is satisfied by two symmetric misunderstandings, so
//! neither arm below compares the new representation against itself:
//!
//! | arm | expected value produced by | is it outside the code under test? |
//! |---|---|---|
//! | `a_real_generated_column_is_cell_identical_to_the_flat_grid` | `lodestone_worldgen`'s own `GeneratedColumn::into_raw()` — the flat `Vec<u16>` + palette, straight out of the generator | **yes**, a different crate, and the exact representation now superseded |
//! | `a_column_read_back_off_disk_is_cell_identical` | the same flat grid, after a `column_to_nbt`/`column_from_nbt` round trip | **yes**, same source, and it also proves `chunk_nbt`'s per-section slicing survived losing `raw_blocks()` |
//!
//! The generator is deterministic per chunk (`OverworldGenerator::column`'s own
//! doc comment, and `chunk.rs`'s `parallel_generation_is_deterministic_and_matches_serial`),
//! which is what lets the reference grid come from a *second* generation of the
//! same coordinates rather than from a clone of the column under test.
//!
//! # Precondition, not decoration
//!
//! Both arms assert the reference column has real variety in it first. A column of
//! pure air would compare equal under a completely broken packer — that is the
//! *world* species of vacuous test (CLAUDE.md's table), the one you cannot find by
//! reading the assertions. The coordinates are on land with a surface, and the
//! preconditions fail rather than skip if the generator ever stops producing that.

use lodestone_server::chunk_nbt;
use lodestone_server::{ChunkColumn, overworld_generator};

/// Chunk coordinates for every arm. Land with a real surface at the default seed —
/// see the `enough_variety` precondition, which is what actually holds it to that.
const CX: i32 = 4;
const CZ: i32 = -7;
const SEED: i64 = 0x5EED_1234;

/// The flat `(y_local * 16 + z) * 16 + x` grid the generator hands over, plus its
/// palette. **This is the representation `ChunkColumn` used to store directly**, so
/// it is both the reference and a faithful model of the old behaviour, and it is
/// produced entirely inside `lodestone-worldgen`.
struct Reference {
    min_y: i32,
    height: i32,
    palette: Vec<String>,
    cells: Vec<u16>,
}

impl Reference {
    fn generate() -> Self {
        let generator = overworld_generator(SEED);
        let (min_y, height, palette, cells, _biomes) = generator.column(CX, CZ).into_raw();
        assert_eq!(
            cells.len(),
            16 * 16 * height as usize,
            "the generator's flat grid must span the whole column, or the comparison \
             below silently covers only part of it"
        );
        Self {
            min_y,
            height,
            palette,
            cells,
        }
    }

    fn state(&self, x: i32, y_local: i32, z: i32) -> &str {
        &self.palette[self.cells[((y_local * 16 + z) * 16 + x) as usize] as usize]
    }

    /// Distinct block states, and how many cells are non-air. Both are the
    /// anti-vacuity numbers: an all-air or single-state column would pass the
    /// comparison under a completely broken packer.
    fn variety(&self) -> (usize, usize) {
        let air = self.palette.iter().position(|p| p == "minecraft:air");
        let non_air = self
            .cells
            .iter()
            .filter(|&&id| Some(id as usize) != air)
            .count();
        let mut seen: Vec<u16> = self.cells.clone();
        seen.sort_unstable();
        seen.dedup();
        (seen.len(), non_air)
    }
}

/// Fails (never skips) unless the reference column has enough content for the
/// comparison to mean something.
fn assert_enough_variety(reference: &Reference) {
    let (distinct, non_air) = reference.variety();
    // Predicted from what a full overworld column is: 98,304 cells over 384 rows,
    // of which a land column's terrain is at least a few tens of thousands, and a
    // real surface pipeline emits well more than a couple of states.
    assert!(
        distinct >= 4,
        "precondition: chunk ({CX}, {CZ}) has only {distinct} distinct block states — \
         the cell comparison below would pass under a packer that ignored ids entirely"
    );
    assert!(
        non_air >= 10_000,
        "precondition: chunk ({CX}, {CZ}) has only {non_air} non-air cells of {} — \
         this is not a land column and the comparison proves little",
        reference.cells.len()
    );
    // And the converse: it must NOT be solid throughout, or every air-collapse
    // path in `chunk_blocks` goes unexercised.
    assert!(
        non_air < reference.cells.len(),
        "precondition: chunk ({CX}, {CZ}) has no air at all, so no section can be \
         uniform and the larger of the two savings is never exercised"
    );
}

/// Compares every cell of `column` against `reference`, reporting the first
/// disagreement with its coordinates rather than only that a mismatch exists.
fn assert_cell_identical(reference: &Reference, column: &ChunkColumn, what: &str) {
    assert_eq!(column.min_y, reference.min_y, "{what}: min_y");
    assert_eq!(column.height, reference.height, "{what}: height");
    let mut compared = 0usize;
    for y_local in 0..reference.height {
        for z in 0..16 {
            for x in 0..16 {
                let expected = reference.state(x, y_local, z);
                let actual = column.block_state(x, y_local + reference.min_y, z);
                assert_eq!(
                    actual, expected,
                    "{what}: cell (x {x}, y {}, z {z}) — the packed grid disagrees with \
                     the generator's own flat grid",
                    y_local + reference.min_y
                );
                compared += 1;
            }
        }
    }
    assert_eq!(
        compared,
        16 * 16 * reference.height as usize,
        "{what}: the loop must visit every cell, not a sample"
    );
}

#[test]
fn a_real_generated_column_is_cell_identical_to_the_flat_grid() {
    let reference = Reference::generate();
    assert_enough_variety(&reference);

    let column = ChunkColumn::from_generated(overworld_generator(SEED).column(CX, CZ));
    assert_cell_identical(&reference, &column, "generated");
}

#[test]
fn a_column_read_back_off_disk_is_cell_identical() {
    // The second half of the check the change needs: `chunk_nbt` lost the
    // `raw_blocks() -> &[u16]` it sliced per section and now calls
    // `append_section_cells`, so the save path's section ordering is newly
    // load-bearing. Round-tripping through the region NBT tree and comparing
    // against the *generator's* grid (not against the column we just wrote)
    // catches a transposition that a write-then-read comparison would not.
    let reference = Reference::generate();
    assert_enough_variety(&reference);

    let column = ChunkColumn::from_generated(overworld_generator(SEED).column(CX, CZ));
    let nbt = chunk_nbt::column_to_nbt(CX, CZ, &column);
    let restored = chunk_nbt::column_from_nbt(&nbt, reference.min_y, reference.height)
        .expect("a column we just wrote must read back");
    assert_cell_identical(&reference, &restored, "round-tripped through region NBT");
}

#[test]
fn the_packed_grid_costs_a_fraction_of_the_flat_one_on_a_real_column() {
    // The saving, as a count of bytes rather than an RSS reading — so it is
    // immune to machine load and to whatever else this process allocated.
    //
    // Both competing hypotheses are computed from outside constants and the
    // measurement must land on one of them, rather than merely being "smaller":
    // the flat representation is unconditionally `16 * 16 * height * 2`, and the
    // packed one has to come in under a quarter of that on real terrain (a
    // 16-bit-per-cell grid where roughly half the sections are air and the rest
    // pack to 5-7 bits cannot plausibly land between the two).
    let reference = Reference::generate();
    assert_enough_variety(&reference);
    let column = ChunkColumn::from_generated(overworld_generator(SEED).column(CX, CZ));

    let flat = 16 * 16 * reference.height as usize * 2;
    let packed = column.blocks_heap_bytes();
    assert_eq!(flat, 196_608, "a full overworld column's flat grid is 192 KiB");
    assert!(
        packed * 4 < flat,
        "the packed grid is {packed} bytes against the flat grid's {flat} — expected at \
         least a 4x cut on real terrain. If this is close to {flat}, the sections are \
         not collapsing air or not narrowing their width; print \
         `column.section_count()` and compare against the terrain surface height."
    );
    // A floor as well as a ceiling: a *zero* here would mean the whole column
    // collapsed to uniform sections, i.e. the terrain vanished, and the cell
    // comparison above would be the only thing standing between that and a green
    // suite.
    assert!(
        packed > 8_192,
        "the packed grid is only {packed} bytes for a column with \
         {} non-air cells — that cannot hold real terrain",
        reference.variety().1
    );
}
