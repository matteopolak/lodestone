//! Composed Nether chunk generation — the second dimension this engine can
//! actually produce terrain for.
//!
//! # What it is
//!
//! The Nether counterpart of [`crate::overworld::OverworldGenerator`]: build one
//! per world seed from `noise_settings/nether.json` plus a [`Resolver`] carrying
//! the Nether's documents, then call [`NetherGenerator::column`] per chunk. It
//! runs vanilla's own stage order — fill, per-quart biome, surface rules, carvers
//! — and holds no version data.
//!
//! It is a **separate type rather than a generalised `OverworldGenerator`**
//! because five of the Overworld generator's stages have no Nether counterpart at
//! all (ore veins, `freeze_top_layer`, the 3×3 vegetation driver, structure
//! placement, the staged neighbour store that exists to serve those drivers), and
//! four of its stages behave differently rather than merely being configured
//! differently. Sharing the type would have meant `if nether` inside the one file
//! this repo's own notes name as a choke point.
//!
//! # How it works, and what differs from the Overworld
//!
//! | | Overworld | Nether |
//! |---|---|---|
//! | RNG family | xoroshiro | **legacy** (`legacy_random_source: true`) |
//! | climate channels | 6 real | temperature + vegetation; the other four are `0.0` constants in the router |
//! | biome noises | `master.fromHashOf(id)` | **`LegacyRandomSource(seed+0)` / `(seed+1)`**, legacy-init `NormalNoise` |
//! | aquifer | `NoiseBasedAquifer` | **disabled** — global fluid picker only |
//! | fluid | water at y<63 | **lava at y<32** |
//! | vertical extent | `min_y -64`, height 384 | `min_y 0`, height 128 |
//! | carver | `cave`, `canyon` | **`nether_cave`** |
//! | bedrock | flag-gated | hardcoded floor y 0–4 *and* roof y 123–127, in the surface rules |
//!
//! Every one of those is data-driven except the carver type, and all of them are
//! reached through machinery that already existed — [`crate::rng::Algorithm`],
//! [`crate::aquifer::AquiferSystem::disabled`], [`crate::carver::CaveConfig::nether`].
//!
//! ## Biomes are two-dimensional here, and that is derived rather than assumed
//!
//! `noise_settings/nether.json`'s `temperature` and `vegetation` are
//! `shifted_noise` with **`y_scale: 0.0`** and `shift_y: 0.0`, so the `y` argument
//! to the underlying noise is the constant `0.0` at every position; the router's
//! other four climate channels are literal `0.0`. A Nether biome is therefore a
//! pure function of `(quartX, quartZ)`, and this generator samples it once per
//! horizontal quart instead of once per 4×4×4 cell. `nether_biomes_do_not_vary_with_y`
//! in `tests/nether_gen.rs` is the gate on that, and it is the reason
//! [`NetherColumn`] carries 16 biomes rather than 128 — **do not copy this shape
//! into a dimension whose climate has a real depth channel**; issue #512 is the
//! record of what broadcasting a biome vertically costs when it is not.
//!
//! ## Structures are here, and they were the island this generator shipped with
//!
//! The first version of this file composed no structure stage at all — no starts,
//! no references, no beardifier, no place step. Every counter read healthy,
//! `bastion_remnant`'s pools loaded, its jigsaw assembly was gated, and it placed
//! **zero blocks anywhere in the game**: its biome tag is Nether-only, so the
//! Overworld's stage could never accept it, and this dimension had no stage to
//! accept it with. Four structures (`bastion_remnant`, `fortress`,
//! `nether_fossil`, `ruined_portal_nether`) sat in that position and the
//! unsupported ledger was silent about all four.
//!
//! What is composed now is the *dimension's* stage sequence, not new structure
//! machinery: [`crate::overworld::structures`]'s
//! [`StructureRefs`](crate::overworld::structures::StructureRefs) product,
//! [`crate::structure::beardifier`] and [`crate::structure::StructureRegistry`]
//! are all dimension-agnostic already, and this file supplies the four things they
//! need that *are* dimension-shaped:
//!
//! | need | the Nether's answer |
//! |---|---|
//! | which structure sets exist here | [`StructureRegistry::new_for_biomes`] over the parameter table's own biome names — vanilla's `hasBiomesForStructureSet`, so a Nether registry loads `bastion`'s pools and no village's |
//! | the height probe | a *disabled* aquifer over the Nether's `final_density`, `min_y 0`, height 128 — not an Overworld-shaped column, and never `sea_level 63` |
//! | the biome the filter reads | [`Self::biome_quarts`]' own sampler at `y = 0`, because this dimension's climate is y-invariant |
//! | memoisation of the 17×17 starts walk | a bounded pure-function memo on this generator, not the Overworld's staged store (which is keyed by that generator's own stage set) |
//!
//! **The bedrock roof and floor are surface-rule products and the height probe
//! does not see them.** `first_occupied_height` reads the *pre-surface* fill,
//! exactly as vanilla's `getBaseHeight` does, so a structure sited near y 127 is
//! sited against noise rather than against the roof it will be buried under. That
//! is vanilla's behaviour and not a gap; it is written down because the opposite
//! assumption is the natural one.
//!
//! ## Decoration is not here
//!
//! Fill, biome, surface, carve and structures are composed. The
//! `UNDERGROUND_ORES` / `VEGETAL_DECORATION` steps that place glowstone, fire,
//! nether wart, crimson/warped vegetation and basalt pillars are **not**. The
//! biome documents already carry the step wiring and every configured/placed
//! feature is bundled, so that is composition work in `crate::feature`, not
//! missing data. See `docs/worldgen-nether.md`.
//!
//! # How to change it
//!
//! * **The fill/surface/carve order is vanilla's and is load-bearing.** Carvers
//!   run over the *post-surface* column, so a carver that exposes netherrack sees
//!   the surface rules' output, not the raw fill. Structures are written *after*
//!   carving, which is where `surface_structures` (step 4) sits.
//! * **`min_gen_y + 31` in `NetherWorldCarver.carveBlock` is not `sea_level`.**
//!   It is hardcoded, and at `min_y 0` it means "lava at y ≤ 31" — one below the
//!   `sea_level 32` the fill uses. Do not unify them.
//! * A biome name this generator can produce must have its carver list resolved
//!   at construction ([`Self::new`]'s `carvers_by_biome` walk), or its columns
//!   silently never carve.
//!
//! # Dependencies
//!
//! [`crate::aquifer`], [`crate::biome`], [`crate::carver`], [`crate::compose`],
//! [`crate::surface`], [`crate::dense_grid`], [`crate::interner`], and
//! `lodestone-worldgen-core`'s density interpreter. Nothing version-specific.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::aquifer::{AquiferSystem, BlockKind};
use crate::biome::{BiomeTable, ClimateSampler};
use crate::carver::{CarveGrid, CarverConfig, NoObserver};
use crate::density::{Builder, Resolver};
use crate::engine::Program;
use crate::interner::StateInterner;
use crate::overworld::structures::{BEARD_REACH, REFS_RADIUS, StructureRefs};
use crate::structure::beardifier::Beardifier;
use crate::structure::{HeightmapKind, StartContext, StructureRegistry, StructureStart};
use crate::surface::{PreState, SurfaceDiff, SurfaceSystem, identity_canon};

/// One generated Nether chunk: the block column plus its 16 horizontal biome
/// quarts.
///
/// Deliberately *not* [`crate::overworld::GeneratedColumn`]: that type carries
/// four products this dimension does not produce (a 4×4×4 biome grid, decoration
/// block entities, a `MOTION_BLOCKING` heightmap, `StageTimes`), and three of
/// them would have to be filled with plausible-looking stand-ins. A caller that
/// needs to serve this over the wire converts explicitly.
#[derive(Debug, Clone)]
pub struct NetherColumn {
    min_y: i32,
    height: i32,
    palette: Vec<String>,
    blocks: Vec<u16>,
    /// Biome id per horizontal quart, row-major `qz * 4 + qx` — the whole answer
    /// for this dimension, see the module doc's 2-D section.
    biome_quarts: [String; 16],
}

impl NetherColumn {
    /// World Y of the lowest block row (0 for the Nether).
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// Number of block rows (128 for the Nether).
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

    /// The biome at horizontal quart `(qx, qz)`, both in `0..4`.
    #[must_use]
    pub fn biome_at_quart(&self, qx: usize, qz: usize) -> &str {
        &self.biome_quarts[qz * 4 + qx]
    }

    /// The biome covering local column `(lx, lz)`.
    #[must_use]
    pub fn biome_at(&self, lx: usize, lz: usize) -> &str {
        self.biome_at_quart(lx >> 2, lz >> 2)
    }

    /// Every distinct biome in this chunk, for a census.
    #[must_use]
    pub fn distinct_biomes(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.biome_quarts.iter().map(String::as_str).collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Count of non-air blocks — the cheapest "did this actually generate
    /// terrain" question, and the one an empty-column bug fails.
    #[must_use]
    pub fn non_air_count(&self) -> usize {
        let air = self
            .palette
            .iter()
            .position(|s| s == "minecraft:air")
            .map(|i| i as u16);
        match air {
            Some(air) => self.blocks.iter().filter(|&&b| b != air).count(),
            None => self.blocks.len(),
        }
    }

    /// The raw parts, for a caller building a chunk packet or a region file.
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

/// A composed, reusable Nether generator. Build once per seed; call
/// [`column`](Self::column) per chunk.
///
/// **Demand-ordered and order-independent.** Nothing here memoises across chunks
/// and no stage reads a neighbouring chunk's product except `applyCarvers`, which
/// re-derives its 17×17 neighbourhood from the seed alone — so `column` is a pure
/// function of `(seed, cx, cz)` and columns may be requested in any order, on any
/// thread, without changing a byte.
#[allow(missing_debug_implementations)]
pub struct NetherGenerator {
    seed: i64,
    slot_count: usize,
    interner: Arc<StateInterner>,
    surface: SurfaceSystem,
    /// `noise_router.final_density`, compiled once. Cloning it per chunk is an
    /// `Arc` bump.
    final_density: Program,
    climate: ClimateSampler,
    table: BiomeTable,
    min_y: i32,
    height: i32,
    sea_level: i32,
    cell_width: i32,
    cell_height: i32,
    default_block: String,
    default_fluid: String,
    default_block_pre: PreState,
    default_fluid_pre: PreState,
    /// `#minecraft:nether_carver_replaceables`. Empty when the resolver supplies
    /// no tag data, in which case carving is a harmless no-op — the same
    /// no-data-supplied convention every other stage here follows.
    carver_replaceable: HashSet<String>,
    carvers_by_biome: HashMap<String, Vec<CarverConfig>>,
    /// The Nether's structure engine, or `None` for a resolver that supplies no
    /// structure sets (every shape/surface fixture in this workspace). `None` makes
    /// every structure stage below an early return, so nothing distinguishes "no
    /// structure data" from "this generator before structures existed".
    structures: Option<StructureRegistry>,
    /// `(cx, cz)` → that chunk's starts. A memo of a **pure function** of
    /// `(seed, cx, cz)`, which is the only reason it may exist at all here: one
    /// `column` call walks the 17×17 [`REFS_RADIUS`] neighbourhood, and without
    /// this a bastion within reach is reassembled once per column that can see it.
    ///
    /// Deliberately *not* the Overworld's [`crate::overworld::store`]: that store's
    /// entry type is the Overworld's own stage set, and its retention ceiling is
    /// sized against a 37×37 pinned closure this dimension does not have.
    ///
    /// **Eviction cannot change a byte of output**, only cost — see
    /// [`STARTS_MEMO_CEILING`] for why the crude policy is sound where the
    /// Overworld needed a view-pinned one.
    starts: Mutex<HashMap<(i32, i32), Arc<Vec<Arc<StructureStart>>>>>,
}

/// Entries [`NetherGenerator::starts`] holds before it is cleared wholesale.
///
/// A 17×17 walk touches 289 chunks, so this is ~28 whole neighbourhoods — enough
/// that a sweep in any locality never evicts a chunk it is about to re-read, and
/// small enough that a long random-access session cannot grow without bound.
///
/// **Clearing the whole map is sound here and would not be in the Overworld.**
/// The Overworld's store holds *stage products* that later stages consume within
/// one `column` call, so dropping one mid-call would silently recompute a
/// neighbour's terrain (which is why that store is view-pinned and counts its
/// evictions). This map holds only starts, each a pure function of
/// `(seed, cx, cz)`: a miss costs a recomputation and returns the identical value,
/// so the worst an eviction can do is make a column slower.
const STARTS_MEMO_CEILING: usize = 8192;

impl NetherGenerator {
    /// Builds the generator for `seed` from `noise_settings/nether.json` and a
    /// [`Resolver`] carrying the Nether's density functions, noises, biome
    /// parameter table, biome documents and configured carvers.
    ///
    /// # Panics
    /// Panics if the resolver's `biome_parameters()` is empty. Unlike the
    /// Overworld generator there is **no fixed-biome fallback**: temperature and
    /// vegetation are the entire Nether biome layout, so a Nether without its
    /// 5-row parameter table is not a degraded world, it is a misconfigured one,
    /// and falling back would produce a uniform `nether_wastes` that looks
    /// plausible in a screenshot.
    #[must_use]
    pub fn new(seed: i64, settings: &Value, resolver: &dyn Resolver) -> Self {
        // The whole point of this phase: the family comes from the document.
        let builder =
            Builder::with_algorithm(seed, crate::rng::Algorithm::from_settings(settings), resolver);
        assert!(
            builder.algorithm().is_legacy(),
            "noise_settings for the Nether must set legacy_random_source: true; \
             with xoroshiro every noise value in the dimension is wrong"
        );

        let router = &settings["noise_router"];
        let interner = Arc::new(StateInterner::new());
        let canon = identity_canon(settings);
        let final_density = Program::compile(&builder.build(&router["final_density"]));
        let surface = SurfaceSystem::new(settings, &builder, &canon, &interner);
        let climate = ClimateSampler::new(settings, &builder);

        let min_y = settings["noise"]["min_y"].as_i64().unwrap_or(0) as i32;
        let height = settings["noise"]["height"].as_i64().unwrap_or(128) as i32;
        let sea_level = settings["sea_level"].as_i64().unwrap_or(32) as i32;
        let (cell_width, cell_height) = crate::aquifer::cell_geometry(settings);

        let default_block = settings["default_block"]["Name"]
            .as_str()
            .unwrap_or("minecraft:netherrack")
            .to_string();
        // The Nether's `default_fluid` carries `{"level": "0"}`, and reading only
        // `Name` would produce `minecraft:lava` where the carver writes
        // `minecraft:lava[level=0]` — two palette entries for one state, and every
        // downstream match on the full string missing for the bare form.
        let default_fluid =
            canonical_state_from_settings(&settings["default_fluid"], "minecraft:lava[level=0]");
        let default_block_pre = PreState::from_name(&interner, &default_block);
        let default_fluid_pre = PreState::from_name(&interner, &default_fluid);

        let raw_table = crate::biome::parse_table(&resolver.biome_parameters());
        assert!(
            !raw_table.is_empty(),
            "the Nether needs its multi-noise parameter table (biome_parameters/nether)"
        );
        let table = BiomeTable::new(raw_table);

        let mut carver_replaceable = HashSet::new();
        {
            let mut seen = HashSet::new();
            crate::compose::resolve_block_tag(
                resolver,
                "minecraft:nether_carver_replaceables",
                &mut carver_replaceable,
                &mut seen,
            );
        }

        let mut carvers_by_biome = HashMap::new();
        // `MultiNoiseBiomeSource.possibleBiomes()` for this dimension, derived from
        // the parameter table rather than written down: a hardcoded list of the
        // Nether's five would be a second copy of the data, and a datapack that
        // added a sixth would silently lose its structures.
        let mut possible_biomes: HashSet<String> = HashSet::new();
        for point in table.iter() {
            possible_biomes.insert(point.biome.clone());
            carvers_by_biome
                .entry(point.biome.clone())
                .or_insert_with(|| crate::compose::build_biome_carvers(resolver, &point.biome));
        }

        // Issue #514's dimension half. Filtered by `possible_biomes` because
        // vanilla filters (`ChunkGeneratorStructureState.createForNormal`), which
        // here means the registry parses `nether_complexes`, `nether_fossils` and
        // `ruined_portals` and loads only `bastion`'s pool graph — not every
        // village's. `is_empty()` → `None`, so a fixture resolver is unaffected.
        let structures = {
            let registry =
                StructureRegistry::new_for_biomes(seed, resolver, Some(&possible_biomes));
            if registry.is_empty() { None } else { Some(registry) }
        };

        // Captured after every `builder.build()` above, which is always a safe
        // bound for any one tree's own sampler.
        let slot_count = builder.slot_count();

        Self {
            seed,
            slot_count,
            interner,
            surface,
            final_density,
            climate,
            table,
            min_y,
            height,
            sea_level,
            cell_width,
            cell_height,
            default_block,
            default_fluid,
            default_block_pre,
            default_fluid_pre,
            carver_replaceable,
            carvers_by_biome,
            structures,
            starts: Mutex::new(HashMap::new()),
        }
    }

    /// The generated column for chunk `(cx, cz)`.
    #[must_use]
    pub fn column(&self, cx: i32, cz: i32) -> NetherColumn {
        let base_x = cx * 16;
        let base_z = cz * 16;

        // Stages 0a/0b, above the fill for the reason
        // `crate::overworld::structures`' module doc gives: the beardifier consults
        // the structure bounds intersecting this chunk, so the bounds have to exist
        // before a single density sample is taken.
        let refs = self.structure_refs(cx, cz);
        let beard = self.beardifier_for(cx, cz, &refs);

        let aquifer = self.build_fill(cx, cz);
        let field = self.fill_stage(&aquifer, base_x, base_z, &beard);
        let heights = self.heights_from_field(&field);
        let biome_quarts = self.biome_quarts(cx, cz);
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);
        let world = self.materialize_world(&field, surface_diff, base_x, base_z);
        let world = self.carve_stage(cx, cz, &aquifer, world);
        // Stage 4b: `surface_structures` sits after carving, and no Nether
        // decoration step runs here yet, so this is the last writer.
        let world = self.structure_place_stage(cx, cz, &refs, world);

        let (palette, blocks) = world.into_palette_and_blocks();
        NetherColumn {
            min_y: self.min_y,
            height: self.height,
            palette,
            blocks,
            biome_quarts,
        }
    }

    /// `Aquifer.createDisabled` bound to this chunk — the Nether's whole fill
    /// decision. See [`AquiferSystem::disabled`].
    fn build_fill(&self, cx: i32, cz: i32) -> AquiferSystem {
        AquiferSystem::disabled(
            self.final_density.clone(),
            self.slot_count,
            self.sea_level,
            BlockKind::Lava,
            self.min_y,
            self.height,
            cx,
            cz,
            self.cell_width,
            self.cell_height,
        )
    }

    fn idx(lx: i32, ly: i32, lz: i32, height: i32) -> usize {
        crate::compose::column_index(lx, ly, lz, height)
    }

    /// `fillFromNoise`, i.e. `add(finalDensity, BeardifierMarker)` —
    /// [`crate::compose::fill_column`], which carries the two-loop rule and the
    /// `-0.0` argument for it.
    ///
    /// The property that matters *here* is what an empty beard means for this
    /// dimension: `nether_fossil` is the Nether's only adaptation-bearing structure
    /// and has no piece generator, so every column takes the no-addition loop and is
    /// bit-identical to the pre-structure generator.
    fn fill_stage(
        &self,
        aquifer: &AquiferSystem,
        base_x: i32,
        base_z: i32,
        beard: &Beardifier,
    ) -> Vec<BlockKind> {
        crate::compose::fill_column(aquifer, base_x, base_z, self.min_y, self.height, beard)
    }

    /// Highest solid Y per column, floored at `sea_level - 1` — the same
    /// `solidTop` definition the Overworld path and `ComposedChunkOracle` use.
    fn heights_from_field(&self, field: &[BlockKind]) -> [i32; 256] {
        crate::compose::solid_top_heights(field, self.min_y, self.height, self.sea_level)
    }

    /// One climate sample per horizontal quart, `Climate.Sampler.sample(quartX,
    /// quartY, quartZ)` → `QuartPos.toBlock` → the parameter list's nearest row.
    ///
    /// `quartY` is passed as 0 because the Nether's climate is y-invariant (module
    /// doc); the gate that keeps this honest is
    /// `nether_biomes_do_not_vary_with_y`.
    ///
    /// **Public because it is the cheap half of `column`** and the parity gate
    /// against the vanilla oracle world's 1,116 stored Nether chunks runs it
    /// alone: 16 climate samples per chunk instead of a whole 32,768-block fill,
    /// which is what makes an exhaustive comparison affordable. It is the same
    /// code path `column` uses, not a reimplementation for the test.
    #[must_use]
    pub fn biome_quarts(&self, cx: i32, cz: i32) -> [String; 16] {
        std::array::from_fn(|i| {
            let qx = cx * 4 + (i % 4) as i32;
            let qz = cz * 4 + (i / 4) as i32;
            let target = self.climate.target(qx * 4, 0, qz * 4);
            self.table.nearest(&target).to_string()
        })
    }

    /// The biome one *source chunk* of the carve neighbourhood resolves to —
    /// vanilla's `carverBiome`, sampled at that chunk's own quart corner and
    /// `y = 0`.
    fn biome_for_carver_source(&self, source_x: i32, source_z: i32) -> &str {
        let target = self.climate.target(source_x * 16, 0, source_z * 16);
        self.table.nearest(&target)
    }

    fn surface_stage(
        &self,
        field: &[BlockKind],
        heights: &[i32; 256],
        biome_quarts: &[String; 16],
        base_x: i32,
        base_z: i32,
    ) -> SurfaceDiff {
        // Re-derived rather than reasoned about, for the reason `overworld::fill`
        // gives: a wrong `PreClass` changes which surface rules fire and still
        // produces a plausible column.
        debug_assert_eq!(
            self.default_block_pre,
            PreState::from_name(&self.interner, &self.default_block),
        );
        debug_assert_eq!(
            self.default_fluid_pre,
            PreState::from_name(&self.interner, &self.default_fluid),
        );

        let pre = |lx: i32, y: i32, lz: i32| -> PreState {
            let ly = y - self.min_y;
            if !(0..self.height).contains(&ly) {
                return PreState::AIR;
            }
            match field[Self::idx(lx, ly, lz, self.height)] {
                BlockKind::Stone => self.default_block_pre,
                // The Nether's `default_fluid` *is* lava, so both fluid arms are
                // the same state here; keeping them separate keeps the match
                // exhaustive over `BlockKind` rather than over this dimension.
                BlockKind::Water | BlockKind::Lava => self.default_fluid_pre,
                BlockKind::Air => PreState::AIR,
            }
        };
        let heightmap = |lx: i32, lz: i32| -> i32 { heights[(lz * 16 + lx) as usize] };
        // `cold_enough_to_snow` is false for every Nether biome (they all declare
        // `temperature: 2.0`), and nothing in `SurfaceRuleData.nether()` reads it
        // — there is no `temperature` condition in the Nether rule tree.
        let biome_at = |lx: i32, lz: i32| -> (&str, bool) {
            (
                biome_quarts[((lz >> 2) * 4 + (lx >> 2)) as usize].as_str(),
                false,
            )
        };

        self.surface
            .build_surface(&pre, &heightmap, &biome_at, base_x, base_z)
    }

    fn materialize_world(
        &self,
        field: &[BlockKind],
        surface_diff: SurfaceDiff,
        base_x: i32,
        base_z: i32,
    ) -> crate::dense_grid::DenseBlockGrid {
        // Point lookups into `surface_diff` in a fixed order, never iteration — see
        // [`crate::compose::materialize_column`] for the palette-order rule and the
        // bug that established it.
        crate::compose::materialize_column(
            &self.interner,
            field,
            &surface_diff,
            base_x,
            base_z,
            self.min_y,
            self.height,
            self.default_block_pre.state,
            self.default_fluid_pre.state,
        )
    }

    /// `applyCarvers` over the post-surface column.
    ///
    /// `top_material` is a constant `None`: `NetherWorldCarver.carveBlock` never
    /// calls it (nor the aquifer, nor the grass tracking) — see
    /// [`crate::carver::CaveConfig::nether`].
    fn carve_stage(
        &self,
        cx: i32,
        cz: i32,
        aquifer: &AquiferSystem,
        world: crate::dense_grid::DenseBlockGrid,
    ) -> crate::dense_grid::DenseBlockGrid {
        let mut grid = CarveGrid::from_dense(world);
        let carvers_for_source =
            |sx: i32, sz: i32| -> Vec<CarverConfig> {
                let biome = self.biome_for_carver_source(sx, sz);
                self.carvers_by_biome.get(biome).cloned().unwrap_or_default()
            };
        let top_material = |_: i32, _: i32, _: i32, _: bool| -> Option<String> { None };
        crate::carver::apply_carvers(
            self.seed,
            cx,
            cz,
            self.min_y,
            self.height,
            &carvers_for_source,
            &mut grid,
            aquifer,
            &self.carver_replaceable,
            &top_material,
            &mut NoObserver,
        );
        grid.into_dense()
    }
}

/// [`StartContext`] over freshly sampled *Nether* noise columns.
///
/// The Overworld's equivalent is `overworld::structures::StartSampler`, and the
/// difference between them is the whole reason this type exists rather than a
/// shared one: every column it samples comes from
/// [`AquiferSystem::disabled`] over the Nether's `final_density` at `min_y 0`,
/// height 128, so a height probe answers against *this* dimension's terrain. A
/// sampler that resolved to an Overworld-shaped column would site a bastion
/// plausibly and wrongly, and nothing in a screenshot would say so.
struct NetherStartSampler<'a> {
    generator: &'a NetherGenerator,
    /// `(cx, cz)` → that chunk's disabled aquifer. `RefCell` because
    /// [`StartContext`] takes `&self` (it is called through a `&dyn` from the
    /// registry) and this is single-threaded per stage invocation. Building one is
    /// the expensive part and a start predicate asks about several columns of the
    /// same chunk.
    aquifers: RefCell<HashMap<(i32, i32), Arc<AquiferSystem>>>,
}

impl NetherStartSampler<'_> {
    fn aquifer(&self, cx: i32, cz: i32) -> Arc<AquiferSystem> {
        if let Some(existing) = self.aquifers.borrow().get(&(cx, cz)) {
            return Arc::clone(existing);
        }
        let built = Arc::new(self.generator.build_fill(cx, cz));
        self.aquifers
            .borrow_mut()
            .insert((cx, cz), Arc::clone(&built));
        built
    }
}

impl StartContext for NetherStartSampler<'_> {
    /// `NoiseBasedChunkGenerator.getFirstOccupiedHeight` — the Y of the topmost
    /// block satisfying the heightmap predicate, or `minY - 1` for a column that
    /// never matches.
    ///
    /// **This reads the pre-surface fill, so it never sees the bedrock roof.** The
    /// Nether's `y 123..=127` bedrock is a `vertical_gradient` surface rule, and
    /// vanilla's own `getBaseHeight` runs against a fresh `NoiseChunk` too — so a
    /// `WORLD_SURFACE_WG` probe here answers "topmost non-air *noise*", which is
    /// exactly the number vanilla's structure placement uses. The Nether's noise is
    /// solid near the roof over most of the dimension, so this is usually a large
    /// number; a structure kind that treats it as "the walkable surface" would be
    /// wrong about this dimension, and vanilla's Nether kinds do not (bastion is
    /// `start_height: {absolute: 33}` and probes nothing).
    fn first_occupied_height(&self, x: i32, z: i32, heightmap: HeightmapKind) -> i32 {
        let generator = self.generator;
        let aquifer = self.aquifer(x >> 4, z >> 4);
        for ly in (0..generator.height).rev() {
            let y = generator.min_y + ly;
            let kind = aquifer.block_at(x, y, z);
            let matched = match heightmap {
                HeightmapKind::WorldSurfaceWg => kind != BlockKind::Air,
                // `blocksMotion`: stone only — lava explicitly excluded, which in
                // this dimension is the difference between the netherrack shelf and
                // the lava sea's surface.
                HeightmapKind::OceanFloorWg => kind == BlockKind::Stone,
            };
            if matched {
                return y;
            }
        }
        generator.min_y - 1
    }

    /// `MultiNoiseBiomeSource.getNoiseBiome(qx, qy, qz)`, written with the real
    /// `qy` rather than the constant 0 [`NetherGenerator::biome_quarts`] uses.
    ///
    /// The two agree by the y-invariance `nether_biomes_do_not_vary_with_y` pins
    /// (`temperature`/`vegetation` are `shifted_noise` with `y_scale: 0.0`), and
    /// spelling it the vanilla way here means the structure biome filter does not
    /// inherit an assumption from a neighbouring optimisation.
    fn biome_at_quart(&self, qx: i32, qy: i32, qz: i32) -> String {
        let target = self.generator.climate.target(qx * 4, qy * 4, qz * 4);
        self.generator.table.nearest(&target).to_string()
    }

    fn sea_level(&self) -> i32 {
        self.generator.sea_level
    }

    /// The real dimension bounds, so a jigsaw structure's `above_bottom` /
    /// `below_top` start height and its `dimension_padding` resolve against a
    /// 0..128 world rather than against the trait's Overworld default. Getting this
    /// from the default would put every `below_top` bastion piece 256 blocks above
    /// the Nether roof.
    fn min_y(&self) -> i32 {
        self.generator.min_y
    }

    fn dimension_height(&self) -> i32 {
        self.generator.height
    }

    /// `isReplaceableByStructures`: air or fluid. Read out of the same per-chunk
    /// aquifer the height probe uses.
    fn is_replaceable_at(&self, x: i32, y: i32, z: i32) -> bool {
        let aquifer = self.aquifer(x >> 4, z >> 4);
        !matches!(aquifer.block_at(x, y, z), BlockKind::Stone)
    }

    /// The four-way fill kind. Here the *default fluid is lava*, so a predicate that
    /// treats "fluid" as water is wrong in this dimension and the distinction is not
    /// academic — it is `ruined_portal_nether`'s whole obsidian/lava test.
    fn block_kind_at(&self, x: i32, y: i32, z: i32) -> BlockKind {
        self.aquifer(x >> 4, z >> 4).block_at(x, y, z)
    }
}

/// The Nether's structure stages — the composition that closes the
/// `dimension:nether_structures` island. See this module's own doc for what is
/// dimension-shaped about them and what is shared.
impl NetherGenerator {
    /// Stage 0a: this chunk's structure starts, memoised in [`Self::starts`].
    ///
    /// Empty and allocation-cheap for a generator with no structure data.
    fn structure_starts_stage(&self, cx: i32, cz: i32) -> Arc<Vec<Arc<StructureStart>>> {
        if let Some(existing) = self
            .starts
            .lock()
            .expect("nether starts memo poisoned")
            .get(&(cx, cz))
        {
            return Arc::clone(existing);
        }
        // Computed **outside** the lock: `starts_at` samples columns and can assemble
        // a whole jigsaw, and holding a single global mutex across that would
        // serialise every generating thread on the memo. Two threads racing the same
        // key both compute, and both compute the same value — the memo is a pure
        // function of `(seed, cx, cz)`, so a duplicated computation costs time and
        // cannot change a byte.
        let computed: Arc<Vec<Arc<StructureStart>>> = Arc::new(match &self.structures {
            None => Vec::new(),
            Some(registry) => {
                let sampler = NetherStartSampler {
                    generator: self,
                    aquifers: RefCell::new(HashMap::new()),
                };
                registry
                    .starts_at(cx, cz, &sampler)
                    .into_iter()
                    .map(Arc::new)
                    .collect()
            }
        });
        let mut memo = self.starts.lock().expect("nether starts memo poisoned");
        if memo.len() >= STARTS_MEMO_CEILING {
            memo.clear();
        }
        memo.insert((cx, cz), Arc::clone(&computed));
        computed
    }

    /// Stage 0b: `createReferences`' 17×17 walk, keeping the starts whose adjusted
    /// box comes within [`BEARD_REACH`] blocks of this chunk.
    ///
    /// Identical in shape to the Overworld's — the widened reach is the
    /// beardifier's own (`Beardifier.forStructuresInChunk`'s
    /// `isCloseToChunk(chunkPos, 12)`), and
    /// [`StructureRefs::packed_by_structure`] re-narrows to vanilla's exact
    /// chunk-box test for the persistence view.
    ///
    /// Not memoised: it is a walk over an already-memoised product and is consumed
    /// exactly once per column, so a second map would carry no work.
    fn structure_refs(&self, cx: i32, cz: i32) -> StructureRefs {
        if self.structures.is_none() {
            return StructureRefs::default();
        }
        let mut entries = Vec::new();
        for sx in (cx - REFS_RADIUS)..=(cx + REFS_RADIUS) {
            for sz in (cz - REFS_RADIUS)..=(cz + REFS_RADIUS) {
                for start in self.structure_starts_stage(sx, sz).iter() {
                    if start
                        .adjusted_bounding_box()
                        .is_close_to_chunk(cx, cz, BEARD_REACH)
                    {
                        entries.push((sx, sz, Arc::clone(start)));
                    }
                }
            }
        }
        StructureRefs { entries }
    }

    /// Stage 0c: this chunk's beard term.
    ///
    /// `nether_fossil` (`terrain_adaptation: beard_thin`) is the dimension's *only*
    /// adaptation-bearing structure and has no piece generator, so today this is
    /// empty for every chunk and [`Self::fill_stage`] takes its no-beard branch —
    /// which is the negative control for the whole change: the Nether's biome and
    /// bedrock parity against the vanilla oracle world is unchanged, by
    /// construction rather than by measurement.
    fn beardifier_for(&self, cx: i32, cz: i32, refs: &StructureRefs) -> Beardifier {
        if self.structures.is_none() {
            return Beardifier::empty();
        }
        Beardifier::for_chunk(cx, cz, refs.adaptation_bearing().map(AsRef::as_ref))
    }

    /// Stage 4b: writes every piece that touches this chunk into `world`.
    ///
    /// Clipping is the grid, not a box: [`crate::dense_grid::DenseBlockGrid::set`]
    /// ignores a write outside this chunk's 16×16 columns, so a piece that straddles
    /// a border writes its own half here and the other half when the neighbour
    /// generates. That is only sound because every piece's position is fixed at
    /// *start* time and every processor draw is position-seeded.
    fn structure_place_stage(
        &self,
        cx: i32,
        cz: i32,
        refs: &StructureRefs,
        mut world: crate::dense_grid::DenseBlockGrid,
    ) -> crate::dense_grid::DenseBlockGrid {
        let Some(registry) = &self.structures else {
            return world;
        };
        let seed = registry.seed();
        let (bx, bz) = (cx * 16, cz * 16);
        for (_, _, start) in &refs.entries {
            if !start.pieces_complete {
                continue;
            }
            // One `referencePos` per start, from its **first** piece's box, before
            // the per-piece loop — `StructureStart.placeInChunk`'s own derivation. It
            // is not a per-piece value and it is not the chunk.
            let reference = crate::structure::jigsaw::reference_position(&start.pieces);
            for piece in &start.pieces {
                if !piece.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15) {
                    continue;
                }
                if let Some(blocks) = &piece.blocks {
                    for block in blocks.iter() {
                        world.set(block.pos[0], block.pos[1], block.pos[2], &block.state);
                    }
                }
                let Some(placement) = &piece.placement else {
                    continue;
                };
                let origin = crate::structure::template::PlaceOrigin {
                    position: placement.position,
                    reference,
                    seed,
                };
                placement
                    .template
                    .place(origin, &placement.settings, &mut world);
                // A `list_pool_element` writes several templates at one position, in
                // document order — `ListPoolElement.place`'s own loop.
                for extra in &piece.extra_placements {
                    let origin = crate::structure::template::PlaceOrigin {
                        position: extra.position,
                        reference,
                        seed,
                    };
                    extra.template.place(origin, &extra.settings, &mut world);
                }
            }
        }
        world
    }

    /// Every start whose origin is `(cx, cz)` and whose piece list is complete —
    /// the set a save file may legitimately carry, and the one a gate asserts on.
    #[must_use]
    pub fn structure_starts(&self, cx: i32, cz: i32) -> Vec<Arc<StructureStart>> {
        self.structure_starts_stage(cx, cz)
            .iter()
            .filter(|start| start.pieces_complete)
            .map(Arc::clone)
            .collect()
    }

    /// Every start whose origin is `(cx, cz)`, including the advisory ones this
    /// engine can place but not build (`fortress`, `nether_fossil`,
    /// `ruined_portal_nether`). This is the *placement* answer — the one to compare
    /// against a vanilla save's `structures.starts` keys.
    #[must_use]
    pub fn structure_starts_including_incomplete(
        &self,
        cx: i32,
        cz: i32,
    ) -> Vec<Arc<StructureStart>> {
        self.structure_starts_stage(cx, cz).to_vec()
    }

    /// This chunk's `structures.References`, narrowed to vanilla's own 16×16
    /// intersection test — the NBT view rather than the beardifier's input.
    #[must_use]
    pub fn structure_references(
        &self,
        cx: i32,
        cz: i32,
    ) -> std::collections::BTreeMap<String, Vec<i64>> {
        let refs = self.structure_refs(cx, cz);
        let (bx, bz) = (cx * 16, cz * 16);
        let mut narrowed = StructureRefs::default();
        for (sx, sz, start) in &refs.entries {
            if start.pieces_complete
                && start.bounding_box.intersects_xz(bx, bz, bx + 15, bz + 15)
            {
                narrowed.entries.push((*sx, *sz, Arc::clone(start)));
            }
        }
        narrowed.packed_by_structure()
    }

    /// The beard term this generator's real starts imply for `(cx, cz)` — the exact
    /// value the production fill uses, so a gate can assert *which* branch the fill
    /// took rather than inferring it from the output.
    #[must_use]
    pub fn beardifier(&self, cx: i32, cz: i32) -> Beardifier {
        let refs = self.structure_refs(cx, cz);
        self.beardifier_for(cx, cz, &refs)
    }

    /// The registry's unsupported ledger, or an empty map for a generator with no
    /// structure data.
    #[must_use]
    pub fn structure_ledger(&self) -> std::collections::BTreeMap<String, String> {
        self.structures
            .as_ref()
            .map(|r| r.unsupported().clone())
            .unwrap_or_default()
    }
}

/// Renders a noise-settings block-state object (`{"Name": …, "Properties": {…}}`)
/// as this engine's canonical `name[k=v,…]` string, properties **sorted by key**.
///
/// The properties are not decoration: `noise_settings/nether.json` carries
/// `"default_fluid": {"Name": "minecraft:lava", "Properties": {"level": "0"}}`,
/// and reading only `Name` yields `minecraft:lava` — a *different string* from the
/// `minecraft:lava[level=0]` [`crate::carver`] writes for the same state. One
/// column would then hold two palette entries for one block and every downstream
/// full-state match would miss for the bare form.
fn canonical_state_from_settings(value: &Value, fallback: &str) -> String {
    let Some(name) = value["Name"].as_str() else {
        return fallback.to_string();
    };
    match value["Properties"].as_object() {
        Some(properties) if !properties.is_empty() => {
            let mut rendered: Vec<String> = properties
                .iter()
                .map(|(key, value)| {
                    let value = value
                        .as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| value.to_string());
                    format!("{key}={value}")
                })
                .collect();
            rendered.sort();
            format!("{name}[{}]", rendered.join(","))
        }
        _ => name.to_string(),
    }
}
