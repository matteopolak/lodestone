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
//! ## Decoration is not here
//!
//! Fill, biome, surface and carve are composed. The `UNDERGROUND_ORES` /
//! `VEGETAL_DECORATION` / `SURFACE_STRUCTURES` steps that place glowstone, fire,
//! nether wart, crimson/warped vegetation and basalt pillars are **not**, and
//! neither are the fortress/bastion/nether-fossil/ruined-portal structures. The
//! biome documents already carry the step wiring and every configured/placed
//! feature is bundled, so that is composition work in `crate::feature`, not
//! missing data. See `docs/worldgen-nether.md`.
//!
//! # How to change it
//!
//! * **The fill/surface/carve order is vanilla's and is load-bearing.** Carvers
//!   run over the *post-surface* column, so a carver that exposes netherrack sees
//!   the surface rules' output, not the raw fill.
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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde_json::Value;

use crate::aquifer::{AquiferSystem, BlockKind};
use crate::biome::{BiomeTable, ClimateSampler};
use crate::carver::{CarveGrid, CarverConfig, NoObserver};
use crate::density::{Builder, Resolver};
use crate::engine::Program;
use crate::interner::{StateId, StateInterner};
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
}

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
        for point in table.iter() {
            carvers_by_biome
                .entry(point.biome.clone())
                .or_insert_with(|| crate::compose::build_biome_carvers(resolver, &point.biome));
        }

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
        }
    }

    /// The generated column for chunk `(cx, cz)`.
    #[must_use]
    pub fn column(&self, cx: i32, cz: i32) -> NetherColumn {
        let base_x = cx * 16;
        let base_z = cz * 16;

        let aquifer = self.build_fill(cx, cz);
        let field = self.fill_stage(&aquifer, base_x, base_z);
        let heights = self.heights_from_field(&field);
        let biome_quarts = self.biome_quarts(cx, cz);
        let surface_diff = self.surface_stage(&field, &heights, &biome_quarts, base_x, base_z);
        let world = self.materialize_world(&field, surface_diff, base_x, base_z);
        let world = self.carve_stage(cx, cz, &aquifer, world);

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
        debug_assert!((0..height).contains(&ly));
        ((ly * 16 + lz) * 16 + lx) as usize
    }

    /// `fillFromNoise`. No beardifier: nothing places a structure in this
    /// dimension yet, and an unconditional `+ 0.0` would flip `-0.0`'s sign bit
    /// for no reason (see `overworld::fill`'s own note).
    fn fill_stage(&self, aquifer: &AquiferSystem, base_x: i32, base_z: i32) -> Vec<BlockKind> {
        let mut field = vec![BlockKind::Air; 16 * 16 * self.height as usize];
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..self.height {
                    field[Self::idx(lx, ly, lz, self.height)] =
                        aquifer.block_at(base_x + lx, self.min_y + ly, base_z + lz);
                }
            }
        }
        field
    }

    /// Highest solid Y per column, floored at `sea_level - 1` — the same
    /// `solidTop` definition the Overworld path and `ComposedChunkOracle` use.
    fn heights_from_field(&self, field: &[BlockKind]) -> [i32; 256] {
        let mut heights = [i32::MIN; 256];
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                let mut top = self.min_y - 1;
                for ly in (0..self.height).rev() {
                    if field[Self::idx(lx, ly, lz, self.height)] == BlockKind::Stone {
                        top = self.min_y + ly;
                        break;
                    }
                }
                heights[(lz * 16 + lx) as usize] = top.max(self.sea_level - 1);
            }
        }
        heights
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
        let mut world = crate::dense_grid::DenseBlockGrid::with_interner(
            Arc::clone(&self.interner),
            base_x,
            self.min_y,
            base_z,
            16,
            self.height,
            16,
            StateId::AIR,
        );
        // Point lookups into `surface_diff` in this fixed order, never iteration
        // — a `DenseBlockGrid`'s palette is built in `set` order, and iterating a
        // hash map here would make two independently built generators produce the
        // same terrain with different bytes (the bug `overworld::fill` records).
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..self.height {
                    let y = self.min_y + ly;
                    let base = match field[Self::idx(lx, ly, lz, self.height)] {
                        BlockKind::Stone => self.default_block_pre.state,
                        BlockKind::Water | BlockKind::Lava => self.default_fluid_pre.state,
                        BlockKind::Air => StateId::AIR,
                    };
                    let state = surface_diff.get(&(lx, y, lz)).copied().unwrap_or(base);
                    world.set_id(base_x + lx, y, base_z + lz, state);
                }
            }
        }
        world
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
