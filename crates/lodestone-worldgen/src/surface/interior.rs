use std::ops::RangeInclusive;

use super::{CompiledRule, CompiledRuleNode, Cond, StateId, SurfaceSystem, NO_RULE_EDGE};

#[derive(Clone, Copy)]
pub(super) struct InteriorCertificate {
    state: StateId,
    min_y: i32,
    max_y: i32,
}

impl InteriorCertificate {
    pub(super) fn prove(
        graph: &CompiledRule,
        conditions: &[Cond],
        min_y: i32,
        height: i32,
        default_block: StateId,
    ) -> Option<Self> {
        let state = StateId::from_state_str("minecraft:netherrack")?;
        if min_y != 0 || height != 128 || default_block != state {
            return None;
        }
        let certificate = Self { state, min_y: 5, max_y: 122 };
        let mut memo = vec![None; graph.nodes.len()];
        let mut visiting = vec![false; graph.nodes.len()];
        certificate.proves_output(graph.entry, graph, conditions, &mut memo, &mut visiting)
            .then_some(certificate)
    }

    fn proves_output(
        self,
        pc: usize,
        graph: &CompiledRule,
        conditions: &[Cond],
        memo: &mut [Option<bool>],
        visiting: &mut [bool],
    ) -> bool {
        if pc == NO_RULE_EDGE || visiting[pc] {
            return false;
        }
        if let Some(result) = memo[pc] {
            return result;
        }
        visiting[pc] = true;
        let result = match &graph.nodes[pc] {
            CompiledRuleNode::Block(state) => *state == self.state,
            CompiledRuleNode::Bandlands(_) | CompiledRuleNode::OreVein { .. } => false,
            CompiledRuleNode::Condition { condition, if_true, if_false }
            | CompiledRuleNode::ColumnCondition { condition, if_true, if_false } => {
                match self.condition_value(&conditions[*condition]) {
                    Some(true) => self.proves_output(*if_true, graph, conditions, memo, visiting),
                    Some(false) => self.proves_output(*if_false, graph, conditions, memo, visiting),
                    None => {
                        self.proves_output(*if_true, graph, conditions, memo, visiting)
                            && self.proves_output(*if_false, graph, conditions, memo, visiting)
                    }
                }
            }
        };
        visiting[pc] = false;
        memo[pc] = Some(result);
        result
    }

    fn condition_value(self, condition: &Cond) -> Option<bool> {
        match condition {
            Cond::StoneDepth { offset: 0, secondary_depth_range: 0, .. } => Some(false),
            Cond::VerticalGradient { true_at_and_below, false_at_and_above, .. }
                if true_at_and_below < false_at_and_above =>
            {
                if self.max_y <= *true_at_and_below {
                    Some(true)
                } else if self.min_y >= *false_at_and_above {
                    Some(false)
                } else {
                    None
                }
            }
            Cond::YAbove {
                anchor_y, surface_depth_multiplier: 0, add_stone_depth: false, ..
            } => {
                if self.min_y >= *anchor_y {
                    Some(true)
                } else if self.max_y < *anchor_y {
                    Some(false)
                } else {
                    None
                }
            }
            Cond::Not(inner) => self.condition_value(inner).map(|value| !value),
            _ => None,
        }
    }

    pub(super) fn state(self) -> StateId { self.state }

    pub(super) fn range(
        self,
        stone_bottom: i32,
        stone_top: i32,
        stone_above_before: i32,
        surface_depth: i32,
    ) -> Option<RangeInclusive<i32>> {
        let threshold = surface_depth.checked_add(1)?.max(1);
        let lo = stone_bottom.checked_add(threshold)?.max(self.min_y);
        let hi = stone_top.checked_add(stone_above_before)?
            .checked_sub(threshold)?.min(stone_top).min(self.max_y);
        (lo <= hi).then_some(lo..=hi)
    }
}

impl SurfaceSystem {
    #[cfg(target_arch = "wasm32")]
    pub(super) fn interior_certificate(&self) -> Option<InteriorCertificate> {
        self.compiled_rule.interior
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn interior_certificate(&self) -> Option<InteriorCertificate> {
        if std::env::var_os("LODESTONE_DISABLE_SURFACE_INTERIOR_SPAN").is_some() {
            None
        } else {
            self.compiled_rule.interior
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::path::Path;

    use lodestone_data::biomes::BuiltinBiome;
    use serde_json::{Value, json};

    use super::*;
    use crate::density::{Builder, NoiseParams, Resolver};
    use crate::rng::{PositionalRandomFactory, RandomSource};
    use crate::surface::{Ctx, EvalCache, PreState, SurfaceBiomeAnswer, SurfaceDiff, NO_WATER};

    struct TinyNoise;

    impl Resolver for TinyNoise {
        fn density_function(&self, id: &str) -> Value { panic!("unexpected density {id}") }

        fn noise(&self, _: &str) -> NoiseParams {
            NoiseParams { first_octave: 0, amplitudes: vec![1.0e-300] }
        }
    }

    fn settings() -> Value {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../lodestone-server/assets/worldgen/noise_settings/nether.json");
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn system(settings: &Value) -> SurfaceSystem {
        let builder = Builder::new(42, &TinyNoise);
        SurfaceSystem::new(settings, &builder, &crate::surface::identity_canon(settings))
    }

    fn scan(
        surface: &SurfaceSystem,
        column: &[PreState; 128],
        biome: impl Fn(i32) -> BuiltinBiome,
    ) -> (SurfaceDiff, Vec<i32>) {
        let calls = RefCell::new(Vec::new());
        let top = column.iter().rposition(|state| *state != PreState::AIR)
            .map_or(-1, |y| y as i32);
        let pre = |x: i32, y: i32, z: i32| {
            if x == 0 && z == 0 && (0..128).contains(&y) {
                column[y as usize]
            } else {
                PreState::AIR
            }
        };
        let answer = |x: i32, y: i32, z: i32| {
            assert_eq!((x, z), (0, 0));
            calls.borrow_mut().push(y);
            SurfaceBiomeAnswer::exact(y, biome(y), false)
        };
        let diff = surface.build_surface_reusing_typed(
            SurfaceDiff::default(), &pre,
            &|x, z| if x == 0 && z == 0 { top } else { -1 },
            &answer, 0, 0,
        );
        (diff, calls.into_inner())
    }

    fn assert_same_writes(actual: &SurfaceDiff, expected: &SurfaceDiff) {
        assert_eq!(actual.changes, expected.changes);
        assert_eq!(actual.column_offsets, expected.column_offsets);
    }

    fn expected_boundary_calls(surface: &SurfaceSystem, fragmented: bool) -> Vec<i32> {
        let floor = surface.master.from_hash_of("minecraft:bedrock_floor").fork_positional();
        let mut expected = if fragmented {
            vec![76, 75, 74, 73, 47, 46, 45, 44, 39, 38, 37, 36]
        } else {
            Vec::new()
        };
        let boundary_start = expected.len();
        for y in (1..=4).rev() {
            let probability = f64::from(5 - y) / 5.0;
            if f64::from(floor.at(0, y, 0).next_float()) >= probability {
                expected.push(y);
            }
        }
        assert_eq!(&expected[boundary_start..], &[4, 3, 2]);
        expected
    }

    #[test]
    fn nether_interior_certificate_proves_compiled_graph_and_exact_span() {
        let surface = system(&settings());
        assert!(!surface.compiled_rule.deep_no_output);
        let certificate = surface.compiled_rule.interior.expect("bundled interior proof");
        assert_eq!(certificate.range(16, 111, 0, 3), Some(20..=107));
        assert_eq!(certificate.range(0, 127, 0, 3), Some(5..=122));
        assert_eq!(certificate.range(16, 111, 0, -4), Some(17..=110));
        assert_eq!(certificate.range(16, 22, 0, 3), None);
        assert_eq!(certificate.range(16, 111, 0, i32::MAX), None);

        let mut fixture = settings();
        fixture["surface_rule"]["sequence"][3]["then_run"]["sequence"][1]
            ["then_run"]["sequence"][1]["if_true"]["min_threshold"] = json!(-1.0);
        let surface = system(&fixture);
        let mut column = [PreState::AIR; 128];
        column[16..=111].fill(PreState::from_name("minecraft:netherrack"));
        assert_eq!(surface.surface_depth(0, 0), 3);
        let (optimized, calls) = scan(&surface, &column, |_| BuiltinBiome::BasaltDeltas);
        let mut ordinary = surface;
        ordinary.compiled_rule.interior = None;
        let (baseline, baseline_calls) = scan(&ordinary, &column, |_| BuiltinBiome::BasaltDeltas);
        assert_same_writes(&optimized, &baseline);
        assert_eq!(baseline_calls.len(), 96);
        assert_eq!(calls, [111, 110, 109, 108, 19, 18, 17, 16]);
        assert_eq!(optimized.len(), 96);
        let netherrack = PreState::from_name("minecraft:netherrack").state;
        let basalt = PreState::from_name("minecraft:basalt[axis=y]").state;
        for y in 16..=111 {
            let expected = if (20..=107).contains(&y) { netherrack } else { basalt };
            assert_eq!(optimized.get(&(0, y, 0)), Some(&expected), "y={y}");
        }
    }

    #[test]
    fn nether_interior_certificate_keeps_bedrock_boundaries_and_fragmented_depths() {
        let mut surface = system(&settings());
        let certificate = surface.compiled_rule.interior.unwrap();
        let stone = PreState::from_name("minecraft:netherrack");
        let fluid = PreState::from_name("minecraft:lava[level=0]");
        let other = PreState::from_name("minecraft:gravel");
        for fragmented in [false, true] {
            let mut column = [stone; 128];
            if fragmented {
                column[40..=43].fill(PreState::AIR);
                column[68..=72].fill(fluid);
                column[88] = other;
            }
            surface.compiled_rule.interior = None;
            let (baseline, _) = scan(&surface, &column, |y| {
                if y % 3 == 0 { BuiltinBiome::SoulSandValley } else { BuiltinBiome::BasaltDeltas }
            });
            surface.compiled_rule.interior = Some(certificate);
            let (optimized, calls) = scan(&surface, &column, |y| {
                if y % 3 == 0 { BuiltinBiome::SoulSandValley } else { BuiltinBiome::BasaltDeltas }
            });
            assert_same_writes(&optimized, &baseline);
            let netherrack = stone.state;
            assert_eq!(optimized.get(&(0, 5, 0)), Some(&netherrack));
            assert_eq!(optimized.get(&(0, 122, 0)), Some(&netherrack));
            for y in [4, 123] {
                assert_eq!(optimized.get(&(0, y, 0)), baseline.get(&(0, y, 0)), "y={y}");
            }
            assert_eq!(calls, expected_boundary_calls(&surface, fragmented));
            if fragmented {
                assert!(optimized.get(&(0, 88, 0)).is_none());
                for y in 40..=43 { assert!(optimized.get(&(0, y, 0)).is_none()); }
                for y in 68..=72 { assert!(optimized.get(&(0, y, 0)).is_none()); }
                assert!(calls.contains(&73));
                assert!(calls.contains(&44));
            } else {
                assert_eq!(optimized.len(), 128);
            }
        }
    }

    #[test]
    fn nether_interior_certificate_matches_actual_conditions_at_negative_depths() {
        let surface = system(&settings());
        let biomes = [BuiltinBiome::BasaltDeltas, BuiltinBiome::SoulSandValley,
            BuiltinBiome::NetherWastes, BuiltinBiome::CrimsonForest, BuiltinBiome::WarpedForest];
        for depth in [-7, -1, 0, 3, 8] {
            let threshold = 1.max(1 + depth);
            for biome in biomes {
                let mut cache = EvalCache::new(surface.xz_cache_slots, surface.y_cache_slots);
                cache.begin_column(false);
                let mut ctx = Ctx {
                    block_x: 0, block_z: 0, surface_depth: depth, surface_secondary: 0.0,
                    min_surface_level: 0, block_y: 5, water_height: NO_WATER,
                    stone_depth_above: threshold + 1, stone_depth_below: threshold + 1,
                    biome: None, typed_biome: Some(SurfaceBiomeAnswer::fixed(biome, false)),
                    biome_builtin: None, biome_at: None, typed_biome_at: None,
                    cache: &mut cache, cache_y: false,
                };
                for y in [5, 21, 64, 121, 122] {
                    ctx.block_y = y;
                    ctx.begin_y();
                    let mut column_conditions = vec![0; surface.conditions.len()];
                    assert_eq!(surface.try_apply_compiled_column(&|_, _| 127, &mut ctx,
                        &mut column_conditions), Some(surface.default_block),
                        "y={y}, depth={depth}, biome={biome:?}");
                }
            }
        }
    }

    #[test]
    fn nether_interior_certificate_packed_scan_keeps_every_scalar_write() {
        let mut surface = system(&settings());
        let certificate = surface.compiled_rule.interior.unwrap();
        let stone = PreState::from_name("minecraft:netherrack");
        let lava = PreState::from_name("minecraft:lava[level=0]");
        for fragmented in [false, true] {
            let mut column = [stone; 128];
            if fragmented {
                column[40..=43].fill(PreState::AIR);
                column[68..=72].fill(lava);
            }
            surface.compiled_rule.interior = None;
            let (baseline, _) = scan(&surface, &column, |y| {
                if y % 2 == 0 { BuiltinBiome::BasaltDeltas } else { BuiltinBiome::SoulSandValley }
            });
            let mut blocks = vec![0u16; 128 * 256];
            for y in 0..128 {
                blocks[y * 256] = match column[y].class {
                    crate::surface::PreClass::Air => 0,
                    crate::surface::PreClass::Stone => 1,
                    crate::surface::PreClass::Fluid => 3,
                };
            }
            let mut carrier = crate::overworld::fill::PackedStateCarrier::surface_fixture(
                blocks, 0, 128, [StateId::AIR, stone.state, lava.state, lava.state],
            );
            let mut heights = [-1; 256];
            heights[0] = 127;
            surface.compiled_rule.interior = Some(certificate);
            let calls = RefCell::new(Vec::new());
            surface.build_surface_reusing_packed_in_place(
                &mut carrier, &heights, &[false; 256],
                &|x, y, z| {
                    assert_eq!((x, z), (0, 0));
                    calls.borrow_mut().push(y);
                    SurfaceBiomeAnswer::exact(y,
                        if y % 2 == 0 { BuiltinBiome::BasaltDeltas } else { BuiltinBiome::SoulSandValley }, false)
                },
                &|_, _, _| {}, 0, 0, surface.preliminary_shared.as_ref(),
            );
            for y in 0..128 {
                let expected = baseline.get(&(0, y, 0)).copied().unwrap_or(column[y as usize].state);
                assert_eq!(carrier.pre_state(0, y, 0).state, expected, "y={y}, fragmented={fragmented}");
            }
            assert_eq!(calls.into_inner(), expected_boundary_calls(&surface, fragmented));
        }
    }

    #[test]
    fn nether_interior_certificate_rejects_custom_outputs_unknown_gates_and_domain() {
        let mut custom = settings();
        custom["surface_rule"]["sequence"].as_array_mut().unwrap().insert(0, json!({
            "type": "minecraft:condition",
            "if_true": { "type": "minecraft:biome", "biome_is": "minecraft:basalt_deltas" },
            "then_run": { "type": "minecraft:block", "result_state": { "Name": "minecraft:gravel" } }
        }));
        let mut surface = system(&custom);
        assert!(surface.compiled_rule.interior.is_none());
        let mut column = [PreState::AIR; 128];
        column[16..=111].fill(PreState::from_name("minecraft:netherrack"));
        let (baseline, _) = scan(&surface, &column, |_| BuiltinBiome::BasaltDeltas);
        surface.compiled_rule.interior = system(&settings()).compiled_rule.interior;
        let (unsafe_result, _) = scan(&surface, &column, |_| BuiltinBiome::BasaltDeltas);
        assert_ne!(unsafe_result.changes, baseline.changes, "unsafe certificate must trip detector");
        assert_eq!(baseline.get(&(0, 64, 0)), Some(&PreState::from_name("minecraft:gravel").state));
        assert_eq!(unsafe_result.get(&(0, 64, 0)), Some(&surface.default_block));

        for change in ["offset", "secondary_depth_range"] {
            let mut custom = settings();
            custom["surface_rule"]["sequence"][3]["then_run"]["sequence"][0]["if_true"][change] = json!(7);
            assert!(system(&custom).compiled_rule.interior.is_none(), "unknown stone gate={change}");
        }
        let mut custom = settings();
        custom["noise"]["min_y"] = json!(-1);
        assert!(system(&custom).compiled_rule.interior.is_none());
        let mut custom = settings();
        custom["noise"]["height"] = json!(256);
        assert!(system(&custom).compiled_rule.interior.is_none());
        let mut custom = settings();
        custom["default_block"]["Name"] = json!("minecraft:stone");
        assert!(system(&custom).compiled_rule.interior.is_none());
    }
}
