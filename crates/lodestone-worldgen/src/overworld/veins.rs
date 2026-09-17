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
//! [`super::OverworldGenerator::materialize_world`] applies it to cells the fill
//! stage reported as solid. Materialisation first builds a request-local
//! candidate list from the toggle and Y-band gates, then resolves the ridged,
//! gap, and positional-RNG gates only for that list. The final placements are
//! consumed in the same z, x, y order as the dense grid walk, so surface
//! building still wins above them and no broad channel cache is retained.
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
//! The sampler geometry is captured from the settings document when these
//! programs are built, so dimensions with different horizontal or vertical
//! sizes retain their own interpolation lattice.
//!
//! **Not yet anchored on a JVM fixture.** A vein-positive dump would be the
//! right gate; what exists today is a generated-column spot check
//! (copper and iron both appear, in their own Y bands, at seed 42). Treat the block
//! choices and thresholds as transcribed-and-reviewed, not measured.

use std::cell::RefCell;

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
    cell_width: i32,
    cell_height: i32,
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
        let (cell_width, cell_height) = crate::aquifer::cell_geometry(settings);
        let id = |name: &str| interner.id_of(name);
        Some(Self {
            toggle: Program::compile(
                &builder
                    .build(&router["vein_toggle"])
                    .expect("bundled vein_toggle density-function document"),
            ),
            ridged: Program::compile(
                &builder
                    .build(&router["vein_ridged"])
                    .expect("bundled vein_ridged density-function document"),
            ),
            gap: Program::compile(
                &builder
                    .build(&router["vein_gap"])
                    .expect("bundled vein_gap density-function document"),
            ),
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
            cell_width,
            cell_height,
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
        // All interpolated vein channels share the dimensions selected by this
        // generator's settings document.
        VeinChunk {
            toggle: NoiseChunkSampler::from_program(
                self.toggle.clone(),
                slots,
                self.cell_width,
                self.cell_height,
                Some(bounds),
            ),
            ridged: NoiseChunkSampler::from_program(
                self.ridged.clone(),
                slots,
                self.cell_width,
                self.cell_height,
                Some(bounds),
            ),
            gap: NoiseChunkSampler::from_program(
                self.gap.clone(),
                slots,
                self.cell_width,
                self.cell_height,
                Some(bounds),
            ),
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

/// A toggle/edge-valid stone cell awaiting the expensive half of the vein
/// decision. The local index is the dense field/grid index, and the original
/// toggle value is retained as bits in an `f64` so richness uses the exact same
/// arithmetic as the scalar path.
#[derive(Debug, Clone, Copy)]
struct VeinCandidate {
    index: u32,
    /// Toggle bits while pending; the resolved state raw id (or
    /// `NO_PLACEMENT`) after the second phase. Keeping both phases in one
    /// allocation avoids a second candidate-sized vector at peak.
    value: u64,
}

thread_local! {
    static VEIN_CANDIDATE_POOL: RefCell<Vec<VeinCandidate>> = const { RefCell::new(Vec::new()) };
    static VEIN_TOGGLE_POOL: RefCell<Vec<f64>> = const { RefCell::new(Vec::new()) };
}

const NO_PLACEMENT: u64 = u64::MAX;

/// The materialisation product: one compact `(local index, state)` pair per
/// candidate, consumed in dense-grid traversal order without a second buffer.
#[derive(Debug)]
pub(super) struct VeinBatch {
    placements: Vec<VeinCandidate>,
    next: usize,
}

impl Drop for VeinBatch {
    fn drop(&mut self) {
        let mut placements = Vec::new();
        std::mem::swap(&mut placements, &mut self.placements);
        VEIN_CANDIDATE_POOL.with(|pool| {
            let mut retained = pool.borrow_mut();
            if placements.capacity() > retained.capacity() {
                *retained = placements;
            }
        });
    }
}

impl VeinBatch {
    #[inline]
    pub(super) fn state_at_index(&mut self, index: usize) -> Option<StateId> {
        let placement = self.placements.get(self.next).copied()?;
        if placement.index as usize != index {
            return None;
        }
        self.next += 1;
        if placement.value == NO_PLACEMENT {
            None
        } else {
            debug_assert!(placement.value <= u16::MAX as u64);
            Some(StateId::from_raw(placement.value as u16))
        }
    }

    pub(super) fn is_consumed(&self) -> bool {
        self.next == self.placements.len()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.placements.len()
    }

    #[cfg(test)]
    fn storage_bytes(&self) -> usize {
        self.placements.capacity() * std::mem::size_of::<VeinCandidate>()
    }
}

impl VeinChunk {
    /// Whether either configured vein type can reach this Y, before sampling a
    /// density channel.
    #[inline]
    pub(super) fn eligible_y(&self, y: i32) -> bool {
        (self.programs.copper.min_y..=self.programs.copper.max_y).contains(&y)
            || (self.programs.iron.min_y..=self.programs.iron.max_y).contains(&y)
    }

    /// Builds candidates in dense-grid order, sampling only `vein_toggle` after
    /// the stone and Y-band gates.
    pub(super) fn prepare_batch(
        &self,
        field: &[crate::aquifer::BlockKind],
        base_x: i32,
        base_z: i32,
        min_y: i32,
        height: i32,
    ) -> VeinBatch {
        self.prepare_batch_with(field.len(), base_x, base_z, min_y, height, |index| {
            field[index] == crate::aquifer::BlockKind::Stone
        })
    }

    /// Packed-fill consumer used by the overworld's in-place materialisation.
    /// Code `1` is the fill stage's stone value; keeping the predicate here
    /// avoids reconstructing a second full `BlockKind` carrier.
    pub(super) fn prepare_batch_packed(
        &self,
        field: &[u16],
        base_x: i32,
        base_z: i32,
        min_y: i32,
        height: i32,
    ) -> VeinBatch {
        self.prepare_batch_with(field.len(), base_x, base_z, min_y, height, |index| {
            field[index] == 1
        })
    }

    fn prepare_batch_with(
        &self,
        field_len: usize,
        base_x: i32,
        base_z: i32,
        min_y: i32,
        height: i32,
        is_stone: impl Fn(usize) -> bool,
    ) -> VeinBatch {
        assert_eq!(field_len, (16 * 16 * height) as usize);
        let eligible_y_count = (0..height)
            .filter(|&ly| self.eligible_y(min_y + ly))
            .count();
        let candidate_capacity = eligible_y_count * 16 * 16;
        let mut candidates = VEIN_CANDIDATE_POOL.with(|pool| {
            let mut candidates = std::mem::take(&mut *pool.borrow_mut());
            candidates.clear();
            if candidates.capacity() < candidate_capacity {
                candidates.reserve(candidate_capacity - candidates.capacity());
            }
            candidates
        });
        // Restrict toggle reads to contiguous stone runs in the union of the
        // two Y bands; ridged and gap are sampled only while resolving
        // candidates.
        let mut toggle_column = VEIN_TOGGLE_POOL.with(|pool| {
            let mut column = std::mem::take(&mut *pool.borrow_mut());
            column.clear();
            if column.capacity() < height as usize {
                column.reserve(height as usize - column.capacity());
            }
            column.resize(height as usize, 0.0);
            column
        });
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                // Walk the fixed-size band union in ascending Y so candidates
                // are ordered for the materialisation cursor. The conditional
                // swap is enough for the two configured bands and avoids a
                // sort/allocation; merging overlapping or adjacent intervals
                // prevents the same index from being appended twice.
                let mut bands = [
                    {
                        let band_min = min_y.max(self.programs.iron.min_y);
                        let band_max = (min_y + height - 1).min(self.programs.iron.max_y);
                        (band_min <= band_max).then_some((band_min, band_max))
                    },
                    {
                        let band_min = min_y.max(self.programs.copper.min_y);
                        let band_max = (min_y + height - 1).min(self.programs.copper.max_y);
                        (band_min <= band_max).then_some((band_min, band_max))
                    },
                ];
                if bands[0].is_none() {
                    bands.swap(0, 1);
                }
                if let (Some((first_min, _)), Some((second_min, _))) = (bands[0], bands[1]) {
                    if second_min < first_min {
                        bands.swap(0, 1);
                    }
                }

                let mut band_index = 0;
                while band_index < bands.len() {
                    let Some((band_min, mut band_max)) = bands[band_index] else {
                        break;
                    };
                    if band_index == 0 {
                        if let Some((next_min, next_max)) = bands[1]
                            && next_min <= band_max.saturating_add(1)
                        {
                            band_max = band_max.max(next_max);
                            band_index = 1;
                        }
                    }
                    let mut ly = band_min - min_y;
                    let last_ly = band_max - min_y;
                    while ly <= last_ly {
                        while ly <= last_ly && !is_stone(((ly * 16 + lz) * 16 + lx) as usize) {
                            ly += 1;
                        }
                        let run_start = ly;
                        while ly <= last_ly && is_stone(((ly * 16 + lz) * 16 + lx) as usize) {
                            ly += 1;
                        }
                        if run_start < ly {
                            let start = run_start as usize;
                            let end = ly as usize;
                            self.toggle.final_density_column(
                                base_x + lx,
                                base_z + lz,
                                min_y + run_start,
                                &mut toggle_column[start..end],
                            );
                            for sampled_ly in run_start..ly {
                                let y = min_y + sampled_ly;
                                if let Some(veininess) = self.candidate_veininess_from_toggle(
                                    toggle_column[sampled_ly as usize],
                                    y,
                                ) {
                                    let index = ((sampled_ly * 16 + lz) * 16 + lx) as usize;
                                    candidates.push(VeinCandidate {
                                        index: u32::try_from(index)
                                            .expect("vein field index exceeds u32"),
                                        value: veininess.to_bits(),
                                    });
                                }
                            }
                        }
                    }
                    band_index += 1;
                }
            }
        }

        // Keep all positional draws for one candidate adjacent and ordered.
        for candidate in &mut candidates {
            let index = candidate.index as usize;
            let lx = (index & 15) as i32;
            let column = index >> 4;
            let lz = (column & 15) as i32;
            let ly = (column >> 4) as i32;
            let state = self.resolve_candidate(
                base_x + lx,
                min_y + ly,
                base_z + lz,
                f64::from_bits(candidate.value),
            );
            candidate.value = state.map_or(NO_PLACEMENT, |state| state.raw() as u64);
        }
        VEIN_TOGGLE_POOL.with(|pool| {
            let mut retained = pool.borrow_mut();
            if toggle_column.capacity() > retained.capacity() {
                *retained = toggle_column;
            }
        });
        VeinBatch {
            placements: candidates,
            next: 0,
        }
    }

    /// Cheap gates shared by the scalar reference and batch path.
    #[inline]
    #[cfg(test)]
    fn candidate_veininess(&self, x: i32, y: i32, z: i32) -> Option<f64> {
        self.candidate_veininess_from_toggle(self.toggle.final_density(x, y, z), y)
    }

    #[inline]
    fn candidate_veininess_from_toggle(&self, veininess: f64, y: i32) -> Option<f64> {
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
            None
        } else {
            Some(veininess)
        }
    }

    /// Resolves the expensive gates and positional draws for one candidate.
    #[inline]
    fn resolve_candidate(&self, x: i32, y: i32, z: i32, veininess: f64) -> Option<StateId> {
        let vein = if veininess > 0.0 {
            self.programs.copper
        } else {
            self.programs.iron
        };
        let veininess_ridged = veininess.abs();
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
            if random.next_float() < CHANCE_OF_RAW_ORE_BLOCK {
                Some(vein.raw_ore_block)
            } else {
                Some(vein.ore)
            }
        } else {
            Some(vein.filler)
        }
    }

    /// Scalar control for the batched vein evaluator.
    #[cfg(test)]
    pub(super) fn state_at(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        self.candidate_veininess(x, y, z)
            .and_then(|veininess| self.resolve_candidate(x, y, z, veininess))
    }
}

#[cfg(test)]
mod tests {
    use super::VeinPrograms;
    use crate::aquifer::BlockKind;
    use crate::density::{Builder, NoiseParams, Resolver};
    use crate::interner::StateInterner;
    use serde_json::Value;

    struct NoReferences;

    impl Resolver for NoReferences {
        fn density_function(&self, id: &str) -> Value {
            panic!("unexpected density-function reference: {id}");
        }

        fn noise(&self, id: &str) -> NoiseParams {
            panic!("unexpected noise reference: {id}");
        }
    }

    #[test]
    fn vein_sampler_uses_settings_cell_geometry() {
        let settings = serde_json::json!({
            "ore_veins_enabled": true,
            "noise": {"size_horizontal": 2, "size_vertical": 1},
            "noise_router": {
                "vein_toggle": {
                    "type": "minecraft:interpolated",
                    "argument": {
                        "type": "minecraft:square",
                        "argument": {
                            "type": "minecraft:y_clamped_gradient",
                            "from_y": 0,
                            "to_y": 8,
                            "from_value": 0.0,
                            "to_value": 1.0
                        }
                    }
                },
                "vein_ridged": {"type": "minecraft:constant", "argument": 0.0},
                "vein_gap": {"type": "minecraft:constant", "argument": 0.0}
            }
        });
        let resolver = NoReferences;
        let builder = Builder::new(0, &resolver);
        let interner = StateInterner::new();
        let programs = VeinPrograms::build(&builder, &settings, &interner)
            .expect("complete vein settings should build");

        assert_eq!((programs.cell_width, programs.cell_height), (8, 4));
        let chunk = programs.for_chunk(builder.slot_count(), 0, 0, 0, 16);
        assert!(!chunk.eligible_y(-61));
        assert!(chunk.eligible_y(-60));
        assert!(chunk.eligible_y(50));
        assert!(!chunk.eligible_y(51));

        // The square of the gradient is 0 at y=0 and 0.25 at y=4. With a
        // four-block cell, y=2 is halfway between those corners.
        assert_eq!(chunk.toggle.final_density(0, 2, 0).to_bits(), 0.125_f64.to_bits());
    }

    #[test]
    fn candidate_batch_matches_scalar_and_stays_compact() {
        let settings = serde_json::json!({
            "ore_veins_enabled": true,
            "noise": {"size_horizontal": 1, "size_vertical": 2},
            "noise_router": {
                "vein_toggle": {
                    "type": "minecraft:interpolated",
                    "argument": {
                        "type": "minecraft:y_clamped_gradient",
                        "from_y": -64,
                        "to_y": 64,
                        "from_value": 0.75,
                        "to_value": 0.9
                    }
                },
                "vein_ridged": {"type": "minecraft:constant", "argument": -1.0},
                "vein_gap": {"type": "minecraft:constant", "argument": 0.0}
            }
        });
        let resolver = NoReferences;
        let builder = Builder::new(42, &resolver);
        let interner = StateInterner::new();
        let programs = VeinPrograms::build(&builder, &settings, &interner)
            .expect("complete vein settings should build");
        let chunk = programs.for_chunk(builder.slot_count(), 128, -64, 0, 16);
        let mut field = vec![BlockKind::Stone; 16 * 16 * 16];
        for (index, block) in field.iter_mut().enumerate() {
            if index % 7 == 0 {
                *block = BlockKind::Water;
            }
        }

        let mut toggle_column = vec![0.0; 16];
        chunk
            .toggle
            .final_density_column(128, -64, 0, &mut toggle_column);
        for (ly, &column_value) in toggle_column.iter().enumerate() {
            assert_eq!(
                column_value.to_bits(),
                chunk.toggle.final_density(128, ly as i32, -64).to_bits(),
                "column toggle changed f64 bits at y={ly}"
            );
        }

        let mut batch = chunk.prepare_batch(&field, 128, -64, 0, 16);
        assert!(batch.len() > 0, "the synthetic toggle must admit candidates");
        assert!(batch.len() < field.len(), "the batch must not become a full volume");
        assert!(
            batch.storage_bytes() <= 2 * field.len() * std::mem::size_of::<f64>(),
            "placement capacity should stay below two f64s per cell, far below three full channels"
        );

        // `from_ordered_state_fn` visits z, x, y. Feed the batch the same order
        // and compare every result with the scalar reference, including the
        // `None` result from the solidness draw.
        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..16i32 {
                    let index = ((ly * 16 + lz) * 16 + lx) as usize;
                    let actual = if field[index] == BlockKind::Stone {
                        batch.state_at_index(index)
                    } else {
                        None
                    };
                    let expected = if field[index] == BlockKind::Stone {
                        chunk.state_at(128 + lx, ly, -64 + lz)
                    } else {
                        None
                    };
                    assert_eq!(actual, expected, "batch differs at local index {index}");
                }
            }
        }
        assert_eq!(batch.next, batch.len(), "the ordered consumer must exhaust the product");
    }

    #[test]
    fn candidate_batch_overlap_control_requires_ascending_union() {
        let settings = serde_json::json!({
            "ore_veins_enabled": true,
            "noise": {"size_horizontal": 1, "size_vertical": 1},
            "noise_router": {
                "vein_toggle": {"type": "minecraft:constant", "argument": 0.75},
                "vein_ridged": {"type": "minecraft:constant", "argument": -1.0},
                "vein_gap": {"type": "minecraft:constant", "argument": 0.0}
            }
        });
        let resolver = NoReferences;
        let builder = Builder::new(42, &resolver);
        let interner = StateInterner::new();
        let programs = VeinPrograms::build(&builder, &settings, &interner)
            .expect("complete vein settings should build");
        let mut chunk = programs.for_chunk(builder.slot_count(), 0, 0, -16, 32);
        // The production bands are disjoint. Widen copper here as a negative
        // control so the old per-band append order exposes duplicate indices.
        chunk.programs.copper.min_y = -12;
        chunk.programs.copper.max_y = 12;
        let field = vec![BlockKind::Stone; 16 * 16 * 32];
        let mut batch = chunk.prepare_batch(&field, 0, 0, -16, 32);
        assert_eq!(batch.len(), 25 * 16 * 16, "the overlap must be materialized once");

        let mut previous = None;
        for candidate in &batch.placements {
            let index = candidate.index as usize;
            let column = index >> 4;
            let traversal_rank = (((column & 15) * 16 + (index & 15)) * 32) + (column >> 4);
            assert!(
                previous.is_none_or(|rank| rank < traversal_rank),
                "overlapping bands must not append duplicate/out-of-order index {} after rank {:?}",
                candidate.index,
                previous
            );
            previous = Some(traversal_rank);
        }

        for lz in 0..16i32 {
            for lx in 0..16i32 {
                for ly in 0..32i32 {
                    let index = ((ly * 16 + lz) * 16 + lx) as usize;
                    assert_eq!(
                        batch.state_at_index(index),
                        chunk.state_at(lx, -16 + ly, lz),
                        "overlap batch differs at local index {index}"
                    );
                }
            }
        }
        assert!(batch.is_consumed(), "the overlap batch must be fully consumed");
    }
}
