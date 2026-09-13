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
//! [`super::OverworldGenerator::biome_cells_stage`] queries each section in
//! `qx` outer, then local `qy`, then `qz` order, matching the reference section
//! fill lifecycle. The resulting cells remain stored as `(qy, qz, qx)` and take
//! one `ClimateSampler::target` + `BiomeTable::nearest` per cell. Typed biome
//! identities are interned into a small per-column palette (`Vec<BiomeRef>` plus
//! `Vec<u16>`), so the 1,536-cell hot path never constructs or compares biome
//! strings. Names are materialised only by the explicit display/serialization
//! compatibility accessor.
//!
//! **The surface array is derived from this grid, not sampled separately.** A
//! quart's surface sample uses `y = (height >> 2) << 2`, which is already
//! quart-aligned, so indexing this grid at that `qy` gives the identical answer.
//! This is the output/decoration surface array; the surface-rule interpreter has
//! a distinct nearby-corner lookup at each block and must not substitute this
//! raw wire grid for that context.
//!
//! ## How to change it, and the gotcha
//!
//! **The per-consumer sampling heights are deliberately divergent and must not be
//! unified.** Carver and ore selection resolve at `y = 0`
//! ([`super::OverworldGenerator::biome_for_carver_source`]); vegetation resolves at
//! the surface. See [`crate::biome`]'s "y = 0 trap" section: at `y = 0` the `depth`
//! gradient is already ≈ +1.0, so a surface `dark_forest` chunk resolves as
//! `lush_caves`. Having a 3-D grid gives each consumer its own correct Y; it does
//! **not** license collapsing them onto one. Surface rules additionally select from
//! eight adjacent quart cells after a seed-derived positional jitter; use
//! `OverworldGenerator::surface_biome_context` rather than this grid for them.
//!
//! ## Cost
//!
//! 96× the biome samples per column. This is the one stage where that is a real
//! multiplier rather than noise, and it is affordable only because U9 replaced the
//! brute-force table scan with vanilla's own indexed `Climate.RTree`
//! ([`crate::biome::tree`]). If this stage ever needs to get cheaper, the lever is
//! that a column's climate targets vary smoothly in Y — not a coarser grid, which
//! would put the cave biomes back out of reach.

use lodestone_data::biomes::{BiomeRef, BuiltinBiome};

/// The typed, private-storage palette used by [`BiomeCells`].
///
/// Built-in entries are one-byte generated enum values inside a four-byte
/// [`BiomeRef`]. Names are not retained beside the palette: a generated biome
/// can resolve its static name at the packet/display boundary, while an
/// extension id must be resolved by the registry that owns that extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BiomePalette {
    entries: Vec<BiomeRef>,
}

impl BiomePalette {
    fn new() -> Self {
        Self { entries: Vec::with_capacity(4) }
    }

    fn push_ref(&mut self, biome: BiomeRef) -> u16 {
        if let Some(index) = self.entries.iter().position(|entry| *entry == biome) {
            return index as u16;
        }
        let index = self.entries.len();
        assert!(index <= u16::MAX as usize, "biome palette exceeds u16 index space");
        self.entries.push(biome);
        index as u16
    }

    /// Typed palette entries in first-use order.
    #[must_use]
    pub fn entries(&self) -> &[BiomeRef] {
        &self.entries
    }

    /// Built-in names in first-use order for a packet or display boundary.
    ///
    /// Generated worldgen tables are strict built-ins, so this conversion is
    /// allocation-free. A caller that admits extension ids must resolve those
    /// ids through its own registry instead of calling this convenience view.
    pub fn builtin_names(&self) -> impl ExactSizeIterator<Item = &'static str> + '_ {
        self.entries.iter().map(|entry| {
            entry
                .builtin_or_none()
                .expect("extension biome requires its owning registry at the name boundary")
                .name()
        })
    }

}

/// One biome identity per quart-position cell of a column. See the module doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BiomeCells {
    /// Distinct typed identities, in first-use order. Index 0 is always present.
    palette: BiomePalette,
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

    /// The typed distinct biome identities in first-use order. A section
    /// encoder should use this for identity work and resolve names only at its
    /// own packet boundary.
    #[must_use]
    pub fn palette_entries(&self) -> &[BiomeRef] {
        self.palette.entries()
    }

    /// The typed palette, for a packet/region encoder that has a registry
    /// mapping available. This deliberately does not expose any parallel
    /// string storage.
    #[must_use]
    pub fn palette(&self) -> &BiomePalette {
        &self.palette
    }

    /// Explicit spelling for callers migrating from the old string slice.
    #[must_use]
    pub fn palette_view(&self) -> &BiomePalette {
        self.palette()
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

    /// Built-in biome name at quart `(qx, qy, qz)` for display/configuration
    /// boundaries. Typed consumers should use [`Self::at_quart_ref`].
    #[must_use]
    pub fn at_quart(&self, qx: usize, qy: usize, qz: usize) -> &'static str {
        self.at_quart_ref(qx, qy, qz)
            .builtin_or_none()
            .expect("extension biome requires its owning registry at the name boundary")
            .name()
    }

    /// Typed biome identity at quart `(qx, qy, qz)`.
    #[must_use]
    pub fn at_quart_ref(&self, qx: usize, qy: usize, qz: usize) -> BiomeRef {
        self.palette.entries()[self.index_at_quart(qx, qy, qz) as usize]
    }

    /// A single-biome column — the fallback for a generator with no climate
    /// table, and what a `ChunkColumn` with no generated data should hold.
    #[must_use]
    pub fn uniform(biome: &str, min_y: i32, height: i32) -> Self {
        Self::uniform_strict(biome, min_y, height)
    }

    /// A strict built-in-only uniform column for production resource data.
    /// Unknown and foreign names panic at this boundary rather than becoming a
    /// silently accepted stringly-typed biome.
    #[must_use]
    pub fn uniform_strict(biome: &str, min_y: i32, height: i32) -> Self {
        let builtin = BuiltinBiome::parse(biome).unwrap_or_else(|error| {
            panic!("failed to parse generated biome resource: {error}")
        });
        Self::uniform_ref(BiomeRef::builtin(builtin), min_y, height)
    }

    /// A uniform column from a typed identity. Extension ids must be resolved
    /// by the caller's registry when this value crosses a serialization
    /// boundary; this constructor itself stores no name.
    #[must_use]
    pub fn uniform_ref(biome: BiomeRef, min_y: i32, height: i32) -> Self {
        let y_quarts = ((height + 3) / 4).max(1) as usize;
        let mut palette = BiomePalette::new();
        let index = palette.push_ref(biome);
        Self {
            palette,
            cells: vec![index; y_quarts * 16],
            y_quarts,
            min_y,
        }
    }

    /// Builds from a closure over every cell, interning as it goes. `f` is called
    /// once per cell in `(qy, qz, qx)` order — the same order the field is laid
    /// out in, so a caller whose sampler has any locality gets it.
    #[cfg(test)]
    pub(crate) fn from_fn<F>(min_y: i32, height: i32, mut f: F) -> Self
    where
        F: FnMut(usize, usize, usize) -> String,
    {
        let y_quarts = ((height + 3) / 4).max(1) as usize;
        let mut palette = BiomePalette::new();
        let mut cells = Vec::with_capacity(y_quarts * 16);
        for qy in 0..y_quarts {
            for qz in 0..4usize {
                for qx in 0..4usize {
                    let name = f(qx, qy, qz);
                    let biome = BuiltinBiome::from_name(&name)
                        .expect("test biome names are generated built-ins");
                    let idx = palette.push_ref(BiomeRef::builtin(biome));
                    cells.push(idx);
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

    /// Builds the same storage layout while invoking the sampler in the
    /// section's production order: section Y, then local X, local Y, local Z.
    /// Palette interning still follows storage order so wire palette bytes stay
    /// independent of the search traversal order.
    #[cfg(test)]
    pub(crate) fn from_fn_section_query_order<F>(min_y: i32, height: i32, mut f: F) -> Self
    where
        F: FnMut(usize, usize, usize) -> String,
    {
        let y_quarts = ((height + 3) / 4).max(1) as usize;
        let mut names = vec![String::new(); y_quarts * 16];
        for section_qy in (0..y_quarts).step_by(4) {
            for qx in 0..4usize {
                for local_y in 0..4usize {
                    let qy = section_qy + local_y;
                    if qy >= y_quarts {
                        break;
                    }
                    for qz in 0..4usize {
                        names[(qy * 4 + qz) * 4 + qx] = f(qx, qy, qz);
                    }
                }
            }
        }
        let mut palette = BiomePalette::new();
        let mut cells = Vec::with_capacity(names.len());
        for name in names {
            let biome = BuiltinBiome::from_name(&name)
                .expect("test biome names are generated built-ins");
            let idx = palette.push_ref(BiomeRef::builtin(biome));
            cells.push(idx);
        }
        Self {
            palette,
            cells,
            y_quarts,
            min_y,
        }
    }

    /// Builds the production storage layout from typed identities while
    /// invoking the sampler in section query order. No per-cell string is
    /// allocated or compared on this path.
    pub fn from_fn_section_query_order_typed<F>(
        min_y: i32,
        height: i32,
        mut f: F,
    ) -> Self
    where
        F: FnMut(usize, usize, usize) -> BiomeRef,
    {
        let y_quarts = ((height + 3) / 4).max(1) as usize;
        let mut values = vec![None; y_quarts * 16];
        for section_qy in (0..y_quarts).step_by(4) {
            for qx in 0..4usize {
                for local_y in 0..4usize {
                    let qy = section_qy + local_y;
                    if qy >= y_quarts {
                        break;
                    }
                    for qz in 0..4usize {
                        values[(qy * 4 + qz) * 4 + qx] = Some(f(qx, qy, qz));
                    }
                }
            }
        }
        let mut palette = BiomePalette::new();
        let mut cells = Vec::with_capacity(values.len());
        for value in values {
            let value = value.expect("section query order visited every biome cell");
            let idx = palette.push_ref(value);
            cells.push(idx);
        }
        Self {
            palette,
            cells,
            y_quarts,
            min_y,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest as _, Sha256};

    fn digest(cells: &BiomeCells) -> String {
        let mut hasher = Sha256::new();
        hasher.update((cells.y_quarts() as u32).to_be_bytes());
        hasher.update(cells.min_y().to_be_bytes());
        for name in cells.palette_view().builtin_names() {
            hasher.update((name.len() as u32).to_be_bytes());
            hasher.update(name.as_bytes());
        }
        for qy in 0..cells.y_quarts() {
            for qz in 0..4 {
                for qx in 0..4 {
                    hasher.update(cells.index_at_quart(qx, qy, qz).to_le_bytes());
                }
            }
        }
        format!("{:x}", hasher.finalize())
    }

    fn pattern(qx: usize, qy: usize, qz: usize) -> BuiltinBiome {
        match (qx + qy * 3 + qz * 5) % 4 {
            0 => BuiltinBiome::Plains,
            1 => BuiltinBiome::DeepDark,
            2 => BuiltinBiome::DripstoneCaves,
            _ => BuiltinBiome::CherryGrove,
        }
    }

    #[test]
    fn typed_cells_match_legacy_palette_cell_and_digest() {
        let legacy = BiomeCells::from_fn_section_query_order(-64, 384, |qx, qy, qz| {
            pattern(qx, qy, qz).name().to_owned()
        });
        let typed = BiomeCells::from_fn_section_query_order_typed(-64, 384, |qx, qy, qz| {
            BiomeRef::builtin(pattern(qx, qy, qz))
        });

        assert_eq!(legacy.palette_entries(), typed.palette_entries());
        for qy in 0..legacy.y_quarts() {
            for qz in 0..4 {
                for qx in 0..4 {
                    assert_eq!(legacy.index_at_quart(qx, qy, qz), typed.index_at_quart(qx, qy, qz));
                }
            }
        }
        assert_eq!(digest(&legacy), digest(&typed));
        assert_eq!(
            digest(&typed),
            "e0cf3a91fce849e192e89fcf17dbdebbaeea475ba2e5c0c78e91cd8debf071b9"
        );
    }

    #[test]
    fn typed_palette_preserves_first_use_order_and_compact_indices() {
        let cells = BiomeCells::from_fn_section_query_order_typed(-64, 8, |_, qy, _| {
            if qy == 0 {
                BiomeRef::builtin(BuiltinBiome::Plains)
            } else {
                BiomeRef::builtin(BuiltinBiome::DeepDark)
            }
        });
        assert_eq!(
            cells.palette_entries(),
            &[
                BiomeRef::builtin(BuiltinBiome::Plains),
                BiomeRef::builtin(BuiltinBiome::DeepDark),
            ]
        );
        assert_eq!(cells.index_at_quart(0, 0, 0), 0);
        assert_eq!(cells.index_at_quart(0, 1, 0), 1);
        assert_eq!(cells.at_quart_ref(0, 1, 0), BiomeRef::builtin(BuiltinBiome::DeepDark));
        assert_eq!(
            cells.palette_view().builtin_names().collect::<Vec<_>>(),
            ["minecraft:plains", "minecraft:deep_dark"]
        );
    }
}
