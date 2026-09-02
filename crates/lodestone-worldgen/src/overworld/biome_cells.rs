//! The full 4×4×4 biome grid, replacing the vertically-broadcast
//! 16-quart surface array as the *authoritative* biome answer for a column.
//!
//! ## What it is
//!
//! [`BiomeCells`] is one biome id per quart-position cell of a served column —
//! `16 × (height / 4)` of them, 1,536 for the standard 384-block overworld
//! column, against the 16 the generator used to produce. That is what
//! vanilla's own multi-noise biome lookup at `(x, y, z)` answers and what
//! vanilla's own level-chunk-section's biome container holds, which is why the wire format has
//! a per-section biome palette at all.
//!
//! ## Why it matters beyond fidelity
//!
//! **No cave biome could previously generate.** `lush_caves`, `dripstone_caves`
//! and `deep_dark` are selected by the `depth` channel at low Y; sampling only at
//! each quart's surface height never queries their climate region, so all three
//! were bundled and unreachable. Underground tint, fog, ambient sound and
//! biome-gated spawning all read the *surface* biome instead.
//!
//! And it is data loss, not only absent generation: re-saving a world written by
//! real vanilla collapses every section's biome container onto the surface value.
//!
//! ## How it works
//!
//! [`super::OverworldGenerator::biome_cells_stage`] walks `qy` outer, then `qz`,
//! then `qx`, and takes one `ClimateSampler::target` + `BiomeTable::nearest` per
//! cell. Ids are interned into a small per-column palette (`Vec<String>` plus
//! `Vec<u16>`) because a column's 1,536 cells only ever hold a handful of
//! distinct biomes — that keeps the struct at ~3 KB rather than 1,536 `String`s.
//!
//! **The surface array is derived from this grid, not sampled separately.** A
//! quart's surface sample uses `y = (height >> 2) << 2`, which is already
//! quart-aligned, so indexing this grid at that `qy` gives the identical answer.
//! `biome_stage` therefore takes this grid as a parameter and samples nothing of
//! its own -- see its doc comment.
//!
//! ## How to change it, and the gotcha
//!
//! **The per-consumer sampling heights are deliberately divergent and must not be
//! unified.** Carver and ore selection resolve at `y = 0`
//! ([`super::OverworldGenerator::biome_for_carver_source`]); vegetation resolves at
//! the surface. See [`crate::biome`]'s "y = 0 trap" section: at `y = 0` the `depth`
//! gradient is already ≈ +1.0, so a surface `dark_forest` chunk resolves as
//! `lush_caves`. Having a 3-D grid gives each consumer its own correct Y; it does
//! **not** license collapsing them onto one.
//!
//! ## Cost
//!
//! 96× the biome samples per column. This is the one stage where that is a real
//! multiplier rather than noise, and it is affordable only because U9 replaced the
//! brute-force table scan with vanilla's own indexed `Climate.RTree`
//! ([`crate::biome::tree`]). If this stage ever needs to get cheaper, the lever is
//! that a column's climate targets vary smoothly in Y — not a coarser grid, which
//! would put the cave biomes back out of reach.

/// One biome id per quart-position cell of a column. See the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BiomeCells {
    /// Distinct biome ids, in first-use order. Index 0 is always present.
    palette: Vec<String>,
    /// `palette` indices, laid out `(qy * 4 + qz) * 4 + qx` — `qy` counting up
    /// from the column's own `min_y >> 2`. Same major-to-minor order as
    /// [`crate::overworld::GeneratedColumn`]'s block field, deliberately, so a
    /// reader that already walks one can walk the other.
    cells: Vec<u16>,
    /// `height / 4`, rounded up.
    y_quarts: usize,
    /// The column's `min_y`, so a caller can convert a world Y without carrying
    /// the generator around.
    min_y: i32,
}

impl BiomeCells {
    /// Number of vertical quart layers.
    #[must_use]
    pub fn y_quarts(&self) -> usize {
        self.y_quarts
    }

    /// The column's lowest block Y.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// The distinct biome ids in this column, first-use order. A section encoder
    /// wants this to build a palette without re-deduplicating.
    #[must_use]
    pub fn palette(&self) -> &[String] {
        &self.palette
    }

    /// Palette index at quart `(qx, qy, qz)`, clamped into range. `qy` counts from
    /// the bottom of the column.
    #[must_use]
    pub fn index_at_quart(&self, qx: usize, qy: usize, qz: usize) -> u16 {
        let qx = qx.min(3);
        let qz = qz.min(3);
        let qy = qy.min(self.y_quarts.saturating_sub(1));
        self.cells[(qy * 4 + qz) * 4 + qx]
    }

    /// Biome id at quart `(qx, qy, qz)`.
    #[must_use]
    pub fn at_quart(&self, qx: usize, qy: usize, qz: usize) -> &str {
        &self.palette[self.index_at_quart(qx, qy, qz) as usize]
    }

    /// A single-biome column — the fallback for a generator with no climate
    /// table, and what a `ChunkColumn` with no generated data should hold.
    #[must_use]
    pub fn uniform(biome: &str, min_y: i32, height: i32) -> Self {
        let y_quarts = ((height + 3) / 4).max(1) as usize;
        Self {
            palette: vec![biome.to_string()],
            cells: vec![0; y_quarts * 16],
            y_quarts,
            min_y,
        }
    }

    /// Builds from a closure over every cell, interning as it goes. `f` is called
    /// once per cell in `(qy, qz, qx)` order — the same order the field is laid
    /// out in, so a caller whose sampler has any locality gets it.
    pub(super) fn from_fn<F>(min_y: i32, height: i32, mut f: F) -> Self
    where
        F: FnMut(usize, usize, usize) -> String,
    {
        let y_quarts = ((height + 3) / 4).max(1) as usize;
        let mut palette: Vec<String> = Vec::with_capacity(4);
        let mut cells = Vec::with_capacity(y_quarts * 16);
        for qy in 0..y_quarts {
            for qz in 0..4usize {
                for qx in 0..4usize {
                    let name = f(qx, qy, qz);
                    // Linear scan, not a HashMap: a column holds a handful of
                    // distinct biomes (measured single digits), so the scan is
                    // shorter than one hash — and this runs 1,536 times per
                    // column, which is exactly where a SipHash would show up.
                    let idx = match palette.iter().position(|p| *p == name) {
                        Some(i) => i,
                        None => {
                            palette.push(name);
                            palette.len() - 1
                        }
                    };
                    cells.push(idx as u16);
                }
            }
        }
        Self {
            palette,
            cells,
            y_quarts,
            min_y,
        }
    }
}
