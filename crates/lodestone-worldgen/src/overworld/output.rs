//! The read-mostly result types [`OverworldGenerator::column`] hands back:
//! [`GeneratedColumn`] (the interned block field) and [`StageTimes`] (the per-stage
//! split `column_timed` measures), plus the adopt-the-dense-grid step between them.
//!
//! Moved here verbatim from `overworld.rs` by U16 Phase A.

use super::OverworldGenerator;

/// Which stages a [`GeneratedColumn`] carries — the wire-facing tag
/// `docs/plans/progressive-chunk-generation.md`'s Stage 1 asks for.
///
/// A two-tier lattice, not a bitset: `Full` is strictly "more generated than"
/// `Shaped`, and there is no third state today. [`OverworldGenerator::column_shaped`]
/// produces `Shaped`; [`OverworldGenerator::column`]/[`OverworldGenerator::column_timed`]
/// produce `Full`. Nothing in this crate ever *downgrades* a column — there is no
/// `Full` → `Shaped` conversion, by construction (see `column_shaped`'s own doc for
/// why it is a pure prefix rather than a strip of a `Full` result).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenStage {
    /// Stages 0a–4: structure starts/refs, fill, biome, surface, materialise,
    /// carve (structure piece placement runs inside this last one — see
    /// [`OverworldGenerator::column_shaped`]). No ores, no vegetation, no
    /// top-layer freeze, no generation-time creature spawns.
    Shaped,
    /// Every stage — today's [`OverworldGenerator::column`].
    Full,
}

impl OverworldGenerator {
    /// Adopts the final dense world grid straight into a [`GeneratedColumn`] —
    /// no re-intern pass (this change's Job 2): a centre-chunk-sized
    /// [`crate::dense_grid::DenseBlockGrid`]'s own `(palette, blocks)` layout is
    /// already identical to [`GeneratedColumn`]'s (`((ly * 16 + lz) * 16 + lx)`,
    /// verified by the `debug_assert!` below rather than merely asserted in a
    /// doc comment).
    ///
    /// `stage` selects what else gets computed. **Only `GenStage::Full` runs the
    /// `SPAWN` stage** — a `Shaped` column must carry zero generation-time
    /// spawn candidates by construction (`docs/plans/progressive-chunk-generation.md`:
    /// "no mobs may exist in a chunk the player cannot interact with"), so
    /// [`OverworldGenerator::column_shaped`] both passes `world`/`block_entities`
    /// that were never touched by vegetation and gets an empty
    /// `spawn_candidates` here regardless of what `world` contains — two
    /// independent reasons for the same empty list, not one.
    pub(super) fn intern_from_dense(
        &self,
        cx: i32,
        cz: i32,
        stage: GenStage,
        world: crate::dense_grid::DenseBlockGrid,
        biome_quarts: [(String, bool); 16],
        biome_cells: super::BiomeCells,
        block_entities: Vec<super::block_entities::GeneratedBlockEntity>,
    ) -> GeneratedColumn {
        let _stage_guard = crate::counters::StageGuard::enter(crate::counters::Stage::Intern);
        debug_assert_eq!(world.bounds().3, 16, "centre chunk width must be 16");
        debug_assert_eq!(world.bounds().4, self.height, "centre chunk height must match the generator's");
        debug_assert_eq!(world.bounds().5, 16, "centre chunk depth must be 16");
        let (palette, blocks) = world.into_palette_and_blocks();
        // The SPAWN stage's part 2. Computed here, alongside
        // the heightmap scan above, for the identical reason — this is the
        // one place already holding the *final* palette/block field and biome
        // quarts, so no earlier stage has to grow a spawn-shaped call site.
        // See `crate::spawn_stage`'s module doc for what these candidates are
        // (and are not) — unconditioned on light/ground, a candidate list a
        // server-side consumer re-validates. Gated on `stage` — see this
        // function's own doc for why `Shaped` must never produce one.
        let spawn_candidates = if matches!(stage, GenStage::Full) {
            let biome_names: [String; 16] = std::array::from_fn(|i| biome_quarts[i].0.clone());
            let height = self.height;
            let min_y = self.min_y;
            let surface_y_at = |lx: usize, lz: usize| -> i32 {
                for ly in (0..height).rev() {
                    let idx = ((ly * 16 + lz as i32) * 16 + lx as i32) as usize;
                    if blocks[idx] != 0 {
                        return min_y + ly + 1;
                    }
                }
                min_y
            };
            let biome_at = |lx: usize, lz: usize| -> String { biome_names[(lz >> 2) * 4 + (lx >> 2)].clone() };
            crate::spawn_stage::spawn_candidates_for_chunk(
                biome_at,
                surface_y_at,
                &self.spawners_by_biome,
                self.seed,
                cx,
                cz,
            )
        } else {
            Vec::new()
        };
        // Computed here rather than in its own stage for two
        // reasons: this is the one place that already holds the *final* palette
        // and block field (so the scan is integer-only — see
        // `motion_blocking_from_palette`), and both `column` and `column_timed`
        // route through this function, so nothing in the pipeline had to grow a
        // call site. Its cost lands in `StageTimes::intern`, which that field's
        // own doc now says. Computed for both stages — it is a pure read of
        // whatever `world` holds, not a claim about *which* stages ran, and
        // nothing downstream consumes it yet regardless (see its own doc).
        let motion_blocking = if self.snow_support.is_empty() {
            None
        } else {
            Some(motion_blocking_from_palette(
                &palette,
                &blocks,
                self.height,
                &self.snow_support,
            ))
        };

        GeneratedColumn {
            min_y: self.min_y,
            height: self.height,
            palette,
            blocks,
            biome_quarts: biome_quarts.map(|(name, _)| name),
            biome_cells,
            block_entities,
            motion_blocking,
            spawn_candidates,
            stage,
        }
    }
}

/// Vanilla's own `MOTION_BLOCKING` heightmap-type registry id, read off its
/// enum constant's own first constructor argument.
///
/// This is the key the 1.21.5+ typed-list heightmap framing carries on the wire
/// (a VarInt registry id, then a VarInt-prefixed long array — see
/// `lodestone_world::heightmap`'s module doc), so a consumer inserts
/// [`GeneratedColumn::motion_blocking_heightmap`] under *this* id and no other.
/// The id is the enum's own first constructor argument, not its ordinal position
/// in the source file — read them off the same line if a second map is added.
pub const MOTION_BLOCKING_HEIGHTMAP_TYPE_ID: u32 = 4;

/// Columns one heightmap covers (16 × 16), and the length of
/// [`GeneratedColumn::motion_blocking_heightmap`]'s array.
pub const HEIGHTMAP_COLUMNS: usize = 256;

/// The `MOTION_BLOCKING` heightmap for a finished column, in vanilla's **stored**
/// form: `first_free_y - min_y`, indexed `lx + lz * 16`.
///
/// # Both halves come from the record definition
///
/// * The **predicate** is a block that blocks motion, or has a non-empty
///   fluid state — vanilla's own definition, read off its own enum constant
///   rather than off a summary, and it is
///   already ported as [`crate::feature::top_layer::SnowSupport::motion_blocking`]
///   over two jar-dumped per-state columns. Nothing new is guessed here.
/// * The **stored value** is `topMatchingY + 1`, offset by `minY`: vanilla's
///   own heightmap priming scans each column downward and stores that value
///   at the first matching block, offset by the chunk's minimum Y (and read
///   back by re-adding that offset). A column with no
///   matching block never gets that store at all, so its slot stays `0`,
///   i.e. `minY` — which is why an all-air column here is `0` and not a sentinel.
///
/// The index is vanilla's own `x + z * 16`, matching
/// `lodestone_world::heightmap::Heightmap::index`, so a consumer can `set` each
/// column straight across with no re-ordering.
///
/// # Why a fresh scan is equivalent to vanilla's incremental maintenance
///
/// The same argument [`crate::feature::top_layer::motion_blocking_first_free`]
/// makes: vanilla primes the heightmaps at the start of the `features` status and
/// maintains them per placed block thereafter, which is an
/// incremental form of exactly this scan. This runs after **every** stage
/// including `TOP_LAYER_MODIFICATION`, so there is nothing left to place and a
/// top-down scan of the finished field lands on the same answer. A future
/// change could ask for incremental maintenance through the region view; that is a
/// *cost* refinement (worldgen-rewrite candidate 3), not a correctness one, and
/// doing it here would not change a single stored height.
///
/// # It is integer-only, deliberately
///
/// [`crate::feature::top_layer::motion_blocking_first_free`] tests the predicate
/// against a **string** per block, which is right for it (it runs inside a stage
/// that holds a `DenseBlockGrid`). Here the palette is already built, so the
/// predicate is evaluated once per *palette entry* — a few dozen times — and the
/// 256-column scan is `u16` compares against a `bool` slice. Reaching for the
/// string form here instead would cost ~200k hash lookups per column for an
/// identical answer.
fn motion_blocking_from_palette(
    palette: &[String],
    blocks: &[u16],
    height: i32,
    support: &crate::feature::top_layer::SnowSupport,
) -> [u16; HEIGHTMAP_COLUMNS] {
    let motion: Vec<bool> = palette
        .iter()
        .map(|state| support.motion_blocking(state))
        .collect();
    let mut out = [0u16; HEIGHTMAP_COLUMNS];
    for lz in 0..16usize {
        for lx in 0..16usize {
            for ly in (0..height as usize).rev() {
                if motion[blocks[(ly * 16 + lz) * 16 + lx] as usize] {
                    // `m + 1`, already relative to `min_y` because `ly` is.
                    out[lx + lz * 16] = (ly + 1) as u16;
                    break;
                }
            }
        }
    }
    out
}

/// Per-stage wall-clock cost of one [`OverworldGenerator::column_timed`] call:
/// **one field per stage the pipeline actually has**, which is what this change
/// asks for.
///
/// # What changed here, and why the old field names were misleading
///
/// This struct previously had four buckets — `shape` / `fluid_heightmap` /
/// `surface` / `intern` — and two of the four did not measure what their name
/// said:
///
/// * **`fluid_heightmap` measured the biome stage and nothing else.** The
///   heightmap (`heights_from_field`) is computed inside the `shape` window, so
///   the bucket between the two timestamps contained exactly
///   [`OverworldGenerator::biome_stage`]. It is now called `biome`.
/// * **`intern` measured materialize + carve + ore + vegetation + interning.**
///   The old doc comment did admit the folding, but the recorded benchmark
///   metric was named `stage_intern_pct`, so the persisted number attributed
///   carvers, ore features and vegetal decoration to "interning" — the precise
///   rot the per-stage cost split exists to stop. Those four now have their own fields.
///
/// So a `stage_intern_pct` figure in `bench-results/generation.jsonl` recorded
/// before this change is **not** interning cost and must not be compared
/// against `stage_intern_pct` recorded after it. The scene strings differ, which
/// is what stops `cargo xtask bench-compare` from silently pairing them.
///
/// # These are cache-cold stage costs, deliberately
///
/// [`OverworldGenerator::column`] reaches its stages through two memo caches
/// ([`OverworldGenerator::pre_ore_stage`] and `post_ore_world`).
/// `column_timed` calls the same stage functions *without* those caches, on
/// purpose: a cache hit costs almost nothing, so a per-stage split taken over
/// memoised calls would attribute ~0% to whichever stage happened to be warm
/// and is not a split of anything. The stage functions, their order and their
/// inputs are identical to `column`'s, so the *output* is identical too —
/// `benches/generation.rs` asserts exactly that against a pair of freshly
/// constructed generators, which is the anti-drift control the "do not create a
/// second pipeline" half of the per-stage cost split asks for.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Clone, Copy)]
pub struct StageTimes {
    /// Building the per-chunk [`crate::aquifer::AquiferSystem`] —
    /// split out of `shape` because the per-stage cost split names the aquifer as a stage whose cost
    /// should be visible on its own terms.
    pub aquifer: std::time::Duration,
    /// The noise router: `fill_stage` (density field, aquifer-participating
    /// fill) plus `heights_from_field`.
    pub shape: std::time::Duration,
    /// Biome sampling (this change's [`OverworldGenerator::biome_stage`]).
    /// Previously, and misleadingly, called `fluid_heightmap`.
    pub biome: std::time::Duration,
    /// Surface rules.
    pub surface: std::time::Duration,
    /// Turning the density field plus the surface diff into a dense block grid.
    pub materialize: std::time::Duration,
    /// Carvers (`crate::carver::apply_carvers`, this change).
    pub carve: std::time::Duration,
    /// The `UNDERGROUND_ORES` 3×3 neighbourhood driver.
    pub ore: std::time::Duration,
    /// Vegetal decoration.
    pub vegetation: std::time::Duration,
    /// `TOP_LAYER_MODIFICATION` — `freeze_top_layer`'s snow and ice. The
    /// first stage to earn its own field rather than being
    /// folded into `intern`, because `docs/plans/worldgen-parity.md` §6 makes a
    /// *quantitative* prediction about it (<5% of composed column cost) that
    /// something has to be able to check.
    pub top_layer: std::time::Duration,
    /// Palette interning, plus this change's `MOTION_BLOCKING` heightmap scan —
    /// and nothing else.
    ///
    /// The heightmap is folded in here rather than given its own field because
    /// it is not a *stage*: it reads the finished palette and block field that
    /// `intern_from_dense` already holds, and no prediction is made about its
    /// cost (unlike `top_layer` above, which has one). If a figure for it is ever
    /// needed, split it here — do not re-derive it from a `column_timed` delta.
    pub intern: std::time::Duration,
}

#[cfg(not(target_arch = "wasm32"))]
impl StageTimes {
    /// Total of every stage (wall-clock, so approximately equal to but not
    /// exactly the same instant range as timing the whole `column()` call).
    ///
    /// Every field is included, so this covers the same span it did when the
    /// struct had four buckets — splitting a bucket does not move the total, and
    /// existing callers that divide one stage by `total()` (e.g.
    /// `lodestone-server`'s `top_layer` share assertion) keep the same meaning.
    #[must_use]
    pub fn total(&self) -> std::time::Duration {
        self.aquifer
            + self.shape
            + self.biome
            + self.surface
            + self.materialize
            + self.carve
            + self.ore
            + self.vegetation
            + self.top_layer
            + self.intern
    }
}

/// A generated 16×`height`×16 block field, block-state strings interned into a
/// small per-column palette.
#[derive(Debug, Clone)]
pub struct GeneratedColumn {
    min_y: i32,
    height: i32,
    palette: Vec<String>,
    blocks: Vec<u16>,
    /// Biome id per horizontal quart, row-major `qz * 4 + qx` —
    /// see [`OverworldGenerator::biome_stage`]. **The surface answer**: this is
    /// the biome a player standing on the column sees, and it is what surface
    /// material, carve and decorate consume.
    ///
    /// It is *not* the biome of the column, and this change is why: broadcasting
    /// it vertically is what made `lush_caves`/`dripstone_caves`/`deep_dark`
    /// unreachable. Read [`Self::biome_cells`] for anything that has a `y`.
    biome_quarts: [String; 16],
    /// The full 4×4×4 biome grid — the authoritative per-cell answer,
    /// and what a per-section biome container on the wire or in a region file
    /// must be built from. See [`super::biome_cells`].
    biome_cells: super::BiomeCells,
    /// Block entities decoration produced inside this chunk, in write
    /// order. Empty for every chunk with no bee nest, which is nearly all of them.
    block_entities: Vec<super::block_entities::GeneratedBlockEntity>,
    /// The `MOTION_BLOCKING` heightmap in vanilla's stored form —
    /// see [`motion_blocking_from_palette`] and
    /// [`Self::motion_blocking_heightmap`].
    ///
    /// `None`, not a zeroed array, when the resolver supplied no
    /// `block_freeze_facts`: the predicate is two jar-dumped per-state columns and
    /// without them there is nothing to evaluate. A zeroed array would be
    /// indistinguishable from "every column is air" and would encode a **wrong**
    /// heightmap, which the save-parity work found is worse than none — vanilla
    /// re-derives any type we omit but trusts one we send.
    ///
    /// Inline rather than boxed on purpose: `blocks` is already ~196 KB per
    /// column, so 512 bytes is noise, and a `Box` would add one allocation per
    /// column to a crate with four allocation-attribution gates.
    motion_blocking: Option<[u16; HEIGHTMAP_COLUMNS]>,
    /// The SPAWN stage's part 2: proposed creature placements —
    /// unconditioned on light/ground legality. See
    /// [`crate::spawn_stage`]'s module doc and [`Self::spawn_candidates`].
    /// Empty for the overwhelming majority of chunks (any biome with no
    /// `creature` spawner entry), exactly like [`Self::block_entities`].
    spawn_candidates: Vec<crate::spawn_stage::GenerationSpawn>,
    /// Which stages produced this column — `docs/plans/progressive-chunk-generation.md`'s
    /// Stage 1 tag. See [`GenStage`] for the lattice and [`Self::stage`] for the
    /// accessor.
    stage: GenStage,
}

impl GeneratedColumn {
    /// Which stages produced this column. A downstream store consults this to
    /// decide whether the column may ever be mutated or persisted — see
    /// [`GenStage`]'s own doc; this crate makes no such decision itself.
    #[must_use]
    pub fn stage(&self) -> GenStage {
        self.stage
    }

    /// World Y of the lowest block row.
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of block rows.
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Canonical block-state string at local `(lx, lz)` in `0..16` and world `y`.
    /// Out-of-range Y is `"minecraft:air"`.
    #[must_use]
    pub fn block_state(&self, lx: usize, y: i32, lz: usize) -> &str {
        let ly = y - self.min_y;
        if !(0..self.height).contains(&ly) {
            return "minecraft:air";
        }
        let idx = ((ly * 16 + lz as i32) * 16 + lx as i32) as usize;
        &self.palette[self.blocks[idx] as usize]
    }

    /// Highest world Y whose block is not air, or `min_y - 1` for an all-air
    /// column. Water counts as non-air (matching `WORLD_SURFACE_WG`).
    #[must_use]
    pub fn top_non_air_y(&self, lx: usize, lz: usize) -> i32 {
        for ly in (0..self.height).rev() {
            let idx = ((ly * 16 + lz as i32) * 16 + lx as i32) as usize;
            if self.blocks[idx] != 0 {
                return self.min_y + ly;
            }
        }
        self.min_y - 1
    }

    /// Number of non-air blocks (telemetry / anti-vacuity).
    #[must_use]
    pub fn non_air_count(&self) -> usize {
        self.blocks.iter().filter(|b| **b != 0).count()
    }

    /// Biome id at local `(lx, lz)` in `0..16` — quart
    /// resolution, broadcast vertically (see [`OverworldGenerator::biome_stage`]),
    /// so the same answer comes back for every `y` at this `(lx, lz)`.
    ///
    /// # Panics
    /// Panics if `lx`/`lz` are not in `0..16`.
    #[must_use]
    pub fn biome_state(&self, lx: usize, lz: usize) -> &str {
        assert!(lx < 16 && lz < 16, "biome_state coordinates out of range");
        &self.biome_quarts[(lz >> 2) * 4 + (lx >> 2)]
    }

    /// Distinct biome count in this column (telemetry / anti-vacuity — a
    /// chunk straddling a biome boundary should report more than one).
    #[must_use]
    pub fn distinct_biome_count(&self) -> usize {
        let mut seen: Vec<&str> = Vec::with_capacity(16);
        for name in &self.biome_quarts {
            if !seen.contains(&name.as_str()) {
                seen.push(name.as_str());
            }
        }
        seen.len()
    }

    /// Consumes the column into its raw parts: `(min_y, height, palette,
    /// blocks, biome_quarts)`, where `blocks[(ly * 16 + lz) * 16 + lx]`
    /// indexes into `palette` (`palette[0] == "minecraft:air"`), `ly = y -
    /// min_y`, and `biome_quarts[qz * 4 + qx]` is this column's biome id for
    /// horizontal quart `(qx, qz)`, constant across `y`.
    ///    /// The full per-cell biome grid. **Read this, not [`Self::biome_quarts_ref`],
    /// for anything that has a `y`** — a per-section biome container, a region-file
    /// `biomes` palette, underground tint/fog, or a spawn rule.
    ///
    /// Deliberately *not* folded into [`Self::into_raw`]: that tuple is destructured
    /// by `lodestone_server::ChunkColumn::from_generated`, and widening it would be
    /// a breaking change to a crate this one must not depend on. A consumer opts in.
    #[must_use]
    pub fn biome_cells(&self) -> &super::BiomeCells {
        &self.biome_cells
    }

    /// The block entities this chunk's decoration produced, with
    /// absolute world positions.
    ///
    /// **Nothing downstream consumes this yet.** `ChunkColumn` has no block-entity
    /// field and the chunk-data packet writes a hardcoded `var_i32(0)`, both outside
    /// this crate — so a generated bee nest still reaches the client empty until
    /// that lands.
    #[must_use]
    pub fn block_entities(&self) -> &[super::block_entities::GeneratedBlockEntity] {
        &self.block_entities
    }

    /// The SPAWN stage's part 2: proposed creature placements for
    /// this chunk — see [`crate::spawn_stage`]'s module doc for what a
    /// candidate is and is not (unconditioned on light/ground). Empty for
    /// most chunks.
    #[must_use]
    pub fn spawn_candidates(&self) -> &[crate::spawn_stage::GenerationSpawn] {
        &self.spawn_candidates
    }

    /// This column's `MOTION_BLOCKING` heightmap, ready to pack — 256
    /// heights in vanilla's **stored** form (`first_free_y - min_y`, so a value in
    /// `0..=height`), indexed `lx + lz * 16`.
    ///
    /// `None` when the generator has no `block_freeze_facts` (every fixture
    /// `Resolver` in this workspace), which is why this unit changes no parity
    /// fixture. See the field's own doc for why that is `None` rather than zeros.
    ///
    /// **Nothing downstream consumes this yet**, the same as
    /// [`Self::block_entities`]: `ChunkColumn` has no heightmap field and
    /// `crates/protocol/v770/src/server_protocol.rs:1465` still writes
    /// `Heightmaps::new().encode(&mut w)` — a well-framed, zero-entry NBT. Both are
    /// outside this crate. The consumer patch is three lines, and the only
    /// non-obvious part is which registry id to key it under:
    /// [`MOTION_BLOCKING_HEIGHTMAP_TYPE_ID`].
    ///
    /// ```text
    /// let mut maps = Heightmaps::new();
    /// let mut map = Heightmap::new(height as u32);
    /// for lz in 0..16 { for lx in 0..16 {
    ///     map.set(lx, lz, u32::from(stored[lx + lz * 16]));
    /// } }
    /// maps.insert(MOTION_BLOCKING_HEIGHTMAP_TYPE_ID, map);
    /// ```
    ///
    /// `Heightmap::new(world_height)` sizes itself with
    /// `height_bits(world_height)` = 9 bits for the overworld's 384, which is the
    /// same `ceillog2(getHeight() + 1)` vanilla's own `BitStorage` uses — so no
    /// width has to be chosen here either.
    #[must_use]
    pub fn motion_blocking_heightmap(&self) -> Option<&[u16; HEIGHTMAP_COLUMNS]> {
        self.motion_blocking.as_ref()
    }

    /// The world Y `Heightmap.getFirstAvailable(MOTION_BLOCKING, lx, lz)` would
    /// return for one column: the first **free** Y above the topmost
    /// motion-blocking-or-fluid block, or `min_y` for a column with none.
    ///
    /// This is the zero-copy hand-off a downstream carrier (e.g. the integrated
    /// server's chunk column) uses to adopt the generated block field without
    /// re-interning every block. The index layout is stable and part of the
    /// contract.
    #[must_use]
    pub fn into_raw(self) -> (i32, i32, Vec<String>, Vec<u16>, [String; 16]) {
        (
            self.min_y,
            self.height,
            self.palette,
            self.blocks,
            self.biome_quarts,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::feature::top_layer::{SnowSupport, StatePredicate};

    /// The three claims a wrong `MOTION_BLOCKING` port gets wrong, on a
    /// hand-built field: the `+1`, the `lx + lz * 16` index, and that an air
    /// column stores `0` (i.e. `min_y`) rather than a sentinel.
    ///
    /// The expected values come from vanilla's own heightmap-priming
    /// derivation — `topMatchingY + 1`, stored offset by `minY` — not from
    /// this function.
    #[test]
    fn motion_blocking_stores_the_first_free_row_at_the_vanilla_index() {
        let height = 8i32;
        // palette 0 must be air (the layout contract `intern_from_dense` asserts).
        let palette = vec![
            "minecraft:air".to_owned(),
            "minecraft:stone".to_owned(),
            "minecraft:water".to_owned(),
            "minecraft:short_grass".to_owned(),
        ];
        let support = SnowSupport {
            // `blocksMotion()` — stone yes, water/short_grass no.
            blocks_motion: StatePredicate::new(
                ["minecraft:stone".to_owned()].into_iter().collect(),
                HashMap::new(),
            ),
            // `!getFluidState().isEmpty()` — water only. This is the half a port
            // that only thinks about solids drops, and it is why a water column
            // must be checked here.
            has_fluid_state: StatePredicate::new(
                ["minecraft:water".to_owned()].into_iter().collect(),
                HashMap::new(),
            ),
            face_full_up: StatePredicate::new(HashSet::new(), HashMap::new()),
            ..SnowSupport::default()
        };

        let idx = |ly: usize, lz: usize, lx: usize| (ly * 16 + lz) * 16 + lx;
        let mut blocks = vec![0u16; 16 * 16 * height as usize];
        // (0,0): stone rows 0..=2 -> first free row 3.
        for ly in 0..3 {
            blocks[idx(ly, 0, 0)] = 1;
        }
        // (5,9): stone row 0, water rows 1..=4 -> water counts, first free 5.
        blocks[idx(0, 9, 5)] = 1;
        for ly in 1..5 {
            blocks[idx(ly, 9, 5)] = 2;
        }
        // (7,2): stone row 0, short_grass row 1 -> short grass does NOT count,
        // so the answer is 1, the row the grass itself occupies.
        blocks[idx(0, 2, 7)] = 1;
        blocks[idx(1, 2, 7)] = 3;
        // (15,15): stone in the very top row -> first free is `height`, the
        // largest value the packing must hold.
        blocks[idx(height as usize - 1, 15, 15)] = 1;

        let map = motion_blocking_from_palette(&palette, &blocks, height, &support);

        assert_eq!(map[0 + 0 * 16], 3, "three stone rows store 2 + 1");
        assert_eq!(map[5 + 9 * 16], 5, "fluid rows count for MOTION_BLOCKING");
        assert_eq!(map[7 + 2 * 16], 1, "a non-blocking, fluid-less block does not");
        assert_eq!(map[15 + 15 * 16], height as u16, "the top row stores `height`");
        // Every untouched column is an all-air column, and vanilla never calls
        // `setHeight` for one — its slot stays 0, meaning `min_y`.
        assert_eq!(
            map.iter().filter(|v| **v == 0).count(),
            HEIGHTMAP_COLUMNS - 4,
            "an air column stores 0 (= min_y), not a sentinel"
        );
        // Transposing the index would put (5,9)'s answer at (9,5).
        assert_eq!(map[9 + 5 * 16], 0, "the index is `lx + lz * 16`, not transposed");
    }

    /// The registry id is read off vanilla's own enum constant's first constructor
    /// argument. Pinned here so a consumer keying a heightmap under the wrong id
    /// — which vanilla would trust rather than re-derive — cannot happen quietly.
    #[test]
    fn motion_blocking_type_id_is_vanillas_own() {
        assert_eq!(MOTION_BLOCKING_HEIGHTMAP_TYPE_ID, 4);
        assert_eq!(HEIGHTMAP_COLUMNS, 16 * 16);
    }
}
