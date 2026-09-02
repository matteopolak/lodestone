//! The ore-vein sampler — the large copper and iron veins.
//!
//! ## What it is
//!
//! A port of vanilla's ore-vein density filler, which runs during fill,
//! behind the aquifer, and
//! replaces the default block with `copper_ore`/`raw_copper_block`/`granite` in
//! `y 0..50` or `deepslate_iron_ore`/`raw_iron_block`/`tuff` in `y -60..-8`.
//!
//! This was a **live parity defect, not a missing feature**: the bundled
//! `noise_settings/overworld.json` already carries `ore_veins_enabled: true` and
//! all three router channels (`vein_toggle`, `vein_ridged`, `vein_gap`, over
//! `minecraft:ore_veininess` / `ore_vein_a` / `ore_vein_b`), and nothing in the
//! engine read any of them.
//!
//! ## How it works
//!
//! Three density programs plus a positional RNG, and no feature-step RNG anywhere
//! — that last part matters, because it means veins cannot desync the ore or
//! vegetation streams no matter what they do. The RNG is vanilla's own
//! ore-vein positional source, i.e. `positional.from_hash_of("minecraft:ore")
//! .fork_positional()`, sampled `at(x, y, z)`; identical in shape to the
//! aquifer's own `"minecraft:aquifer"` factory next to it in
//! [`super::OverworldGenerator::new`].
//!
//! [`super::OverworldGenerator::materialize_world`] applies it: for every cell the
//! fill stage reported as solid **and** the surface rules did not rewrite, ask
//! [`VeinChunk::state_at`]. That placement is what mirrors vanilla's own
//! material-rule ordering — veins only see positions where the aquifer returned
//! the default block, and surface building still wins above them.
//!
//! ## How to change it, and the one named approximation
//!
//! `vein_toggle` and `vein_ridged` are `minecraft:interpolated` in the JSON, so
//! they are evaluated through [`NoiseChunkSampler`] — the same cell-interpolating
//! wrapper `final_density` uses — rather than pointwise. That is the whole reason
//! [`VeinChunk`] exists per chunk instead of the programs being sampled directly:
//! a pointwise `Density::compute` would silently drop the interpolation and
//! produce veins in the right *places* with the wrong *shape*, which is the
//! hardest kind of wrong to notice.
//!
//! **Not yet anchored on a JVM fixture.** A vein-positive dump would be the
//! right gate; what exists today is a generated-column spot check
//! (copper and iron both appear, in their own Y bands, at seed 42). Treat the block
//! choices and thresholds as transcribed-and-reviewed, not measured.

use crate::density::NoiseChunkSampler;
use crate::engine::{Bounds, Program};
use crate::interner::StateId;
use crate::math::clamped_map;
use crate::rng::{PositionalRandomFactory, RandomSource, AnyPositionalFactory};

/// Vanilla's own constants, named to match.
const VEININESS_THRESHOLD: f64 = 0.4;
const EDGE_ROUNDOFF_BEGIN: f64 = 20.0;
const MAX_EDGE_ROUNDOFF: f64 = -0.2;
const VEIN_SOLIDNESS: f32 = 0.7;
const MIN_RICHNESS: f64 = 0.1;
const MAX_RICHNESS: f64 = 0.3;
const MAX_RICHNESS_THRESHOLD: f64 = 0.6;
const CHANCE_OF_RAW_ORE_BLOCK: f32 = 0.02;
const SKIP_ORE_IF_GAP_NOISE_IS_BELOW: f64 = -0.3;

/// Vanilla's own per-vein-type record — its three block states (pre-interned) and Y band.
#[derive(Debug, Clone, Copy)]
struct VeinType {
    ore: StateId,
    raw_ore_block: StateId,
    filler: StateId,
    min_y: i32,
    max_y: i32,
}

/// The per-generator half: compiled programs, the positional factory and the two
/// vein types' interned states. Built once per world, cloned per chunk only as
/// `Arc` bumps inside [`Program`].
#[allow(missing_debug_implementations)]
#[derive(Clone)]
pub(super) struct VeinPrograms {
    toggle: Program,
    ridged: Program,
    gap: Program,
    positional: AnyPositionalFactory,
    copper: VeinType,
    iron: VeinType,
}

impl VeinPrograms {
    /// `None` when the settings say `ore_veins_enabled: false` or any of the three
    /// router channels is absent — the same "no data supplied, degrade quietly"
    /// convention every other composition path in this crate follows. A settings
    /// document without veins must generate a vein-free world, not panic.
    pub(super) fn build(
        builder: &crate::density::Builder,
        settings: &serde_json::Value,
        interner: &crate::interner::StateInterner,
    ) -> Option<Self> {
        if !settings["ore_veins_enabled"].as_bool().unwrap_or(false) {
            return None;
        }
        let router = &settings["noise_router"];
        for key in ["vein_toggle", "vein_ridged", "vein_gap"] {
            if router.get(key).is_none() {
                return None;
            }
        }
        let id = |name: &str| interner.id_of(name);
        Some(Self {
            toggle: Program::compile(&builder.build(&router["vein_toggle"])),
            ridged: Program::compile(&builder.build(&router["vein_ridged"])),
            gap: Program::compile(&builder.build(&router["vein_gap"])),
            positional: {
                let mut src = builder.positional_factory().from_hash_of("minecraft:ore");
                src.fork_positional()
            },
            copper: VeinType {
                ore: id("minecraft:copper_ore"),
                raw_ore_block: id("minecraft:raw_copper_block"),
                filler: id("minecraft:granite"),
                min_y: 0,
                max_y: 50,
            },
            iron: VeinType {
                ore: id("minecraft:deepslate_iron_ore"),
                raw_ore_block: id("minecraft:raw_iron_block"),
                filler: id("minecraft:tuff"),
                min_y: -60,
                max_y: -8,
            },
        })
    }

    /// Binds these programs to one chunk's query box. Called once per
    /// `materialize_world`, matching vanilla's one `NoiseChunk` per chunk — the
    /// interpolation caches inside [`NoiseChunkSampler`] assume it.
    pub(super) fn for_chunk(
        &self,
        slots: usize,
        min_block_x: i32,
        min_block_z: i32,
        min_y: i32,
        height: i32,
    ) -> VeinChunk {
        let bounds = Bounds {
            x: (min_block_x, min_block_x + 15),
            y: (min_y, min_y + height - 1),
            z: (min_block_z, min_block_z + 15),
        };
        // Cell width/height are vanilla's `NoiseSettings` 4/8 — the same pair
        // `crate::aquifer` passes, and the reason they are not read from the
        // settings here is that nothing in this crate reads them from there yet.
        VeinChunk {
            toggle: NoiseChunkSampler::from_program(self.toggle.clone(), slots, 4, 8, Some(bounds)),
            ridged: NoiseChunkSampler::from_program(self.ridged.clone(), slots, 4, 8, Some(bounds)),
            gap: NoiseChunkSampler::from_program(self.gap.clone(), slots, 4, 8, Some(bounds)),
            programs: self.clone(),
        }
    }
}

/// One chunk's bound vein samplers. See [`VeinPrograms::for_chunk`].
#[allow(missing_debug_implementations)]
pub(super) struct VeinChunk {
    toggle: NoiseChunkSampler,
    ridged: NoiseChunkSampler,
    gap: NoiseChunkSampler,
    programs: VeinPrograms,
}

impl VeinChunk {
    /// Vanilla's own vein density filler, verbatim in order: the Y-band
    /// test, the edge roundoff, the solidness roll, the ridged test, the richness
    /// roll and the raw-ore roll — all three RNG draws off one positional source,
    /// in that order, because they share it.
    ///
    /// `None` means "leave the default block alone", vanilla's `null`.
    pub(super) fn state_at(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        let veininess = self.toggle.final_density(x, y, z);
        let vein = if veininess > 0.0 {
            self.programs.copper
        } else {
            self.programs.iron
        };
        let veininess_ridged = veininess.abs();
        let distance_from_top = vein.max_y - y;
        let distance_from_bottom = y - vein.min_y;
        if distance_from_bottom < 0 || distance_from_top < 0 {
            return None;
        }
        let distance_from_edge = distance_from_top.min(distance_from_bottom);
        let edge_roundoff = clamped_map(
            f64::from(distance_from_edge),
            0.0,
            EDGE_ROUNDOFF_BEGIN,
            MAX_EDGE_ROUNDOFF,
            0.0,
        );
        if veininess_ridged + edge_roundoff < VEININESS_THRESHOLD {
            return None;
        }
        let mut random = self.programs.positional.at(x, y, z);
        if random.next_float() > VEIN_SOLIDNESS {
            return None;
        }
        if self.ridged.final_density(x, y, z) >= 0.0 {
            return None;
        }
        let richness = clamped_map(
            veininess_ridged,
            VEININESS_THRESHOLD,
            MAX_RICHNESS_THRESHOLD,
            MIN_RICHNESS,
            MAX_RICHNESS,
        );
        if f64::from(random.next_float()) < richness
            && self.gap.final_density(x, y, z) > SKIP_ORE_IF_GAP_NOISE_IS_BELOW
        {
            Some(if random.next_float() < CHANCE_OF_RAW_ORE_BLOCK {
                vein.raw_ore_block
            } else {
                vein.ore
            })
        } else {
            Some(vein.filler)
        }
    }
}
