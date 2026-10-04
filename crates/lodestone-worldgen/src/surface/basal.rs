use super::{Cond, Rule, StateId, SurfaceSystem};
use lodestone_data::biomes::BuiltinBiome;
use lodestone_data::block::Block;

#[derive(Clone, Copy)]
pub(super) struct BasalCertificate {
    state: StateId,
}

impl BasalCertificate {
    pub(super) fn prove(
        rule: &Rule,
        conditions: &[Cond],
        min_y: i32,
        height: i32,
        default_block: StateId,
    ) -> Option<Self> {
        if min_y != -64 || height != 384 || default_block != Block::Stone.default_state()
            || !conditions.iter().all(Self::depth_one_condition)
        {
            return None;
        }
        let Rule::Sequence(arms) = rule else { return None };
        let [floor, upper, sulfur, basal] = arms.as_slice() else { return None };
        let Rule::Condition(floor_condition, floor_result) = floor else { return None };
        if !matches!(conditions[*floor_condition], Cond::VerticalGradient {
            true_at_and_below: -64, false_at_and_above: -59, ..
        }) || !matches!(floor_result.as_ref(), Rule::Block(state)
            if *state == Block::Bedrock.default_state())
        {
            return None;
        }
        let Rule::Condition(upper_condition, _) = upper else { return None };
        if !matches!(conditions[*upper_condition], Cond::AbovePreliminarySurface) {
            return None;
        }
        let Rule::Condition(sulfur_condition, _) = sulfur else { return None };
        if !matches!(&conditions[*sulfur_condition], Cond::BiomeIs { set, .. }
            if set.is_exact_builtin(BuiltinBiome::SulfurCaves))
        {
            return None;
        }
        let Rule::Condition(basal_condition, basal_result) = basal else { return None };
        let state = StateId::from_state_str("minecraft:deepslate[axis=y]")?;
        if !matches!(conditions[*basal_condition], Cond::VerticalGradient {
            true_at_and_below: 0, false_at_and_above: 8, ..
        }) || !matches!(basal_result.as_ref(), Rule::Block(result) if *result == state)
        {
            return None;
        }
        Some(Self { state })
    }

    fn depth_one_condition(condition: &Cond) -> bool {
        match condition {
            Cond::StoneDepth {
                offset, add_surface_depth, secondary_depth_range, ceiling, ..
            } => !ceiling || (*offset == 0 && !add_surface_depth && *secondary_depth_range == 0),
            Cond::Not(inner) => Self::depth_one_condition(inner),
            Cond::AbovePreliminarySurface
            | Cond::BiomeIs { .. }
            | Cond::NoiseThreshold { .. }
            | Cond::Steep { .. }
            | Cond::Temperature { .. }
            | Cond::Hole { .. }
            | Cond::VerticalGradient { .. }
            | Cond::Water { .. }
            | Cond::YAbove { .. } => true,
        }
    }

    pub(super) fn state(self) -> StateId { self.state }
}

impl SurfaceSystem {
    #[cfg(target_arch = "wasm32")]
    pub(super) fn basal_certificate(&self) -> Option<BasalCertificate> {
        self.compiled_rule.basal
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn basal_certificate(&self) -> Option<BasalCertificate> {
        if std::env::var_os("LODESTONE_DISABLE_SURFACE_BASAL_FUSION").is_some() {
            None
        } else {
            self.compiled_rule.basal
        }
    }
}
