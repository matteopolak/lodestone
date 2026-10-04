use super::{BuiltinBiome, CompiledRuleNode, Cond, NO_RULE_EDGE};

const NO_ROW: u32 = u32::MAX;

pub(super) struct BiomeResidual {
    rows: Vec<u32>,
    destinations: Vec<u32>,
}

pub(super) struct BiomeRow<'a>(&'a [u32]);

impl BiomeRow<'_> {
    #[inline]
    pub(super) fn destination(&self, biome: BuiltinBiome) -> usize {
        let destination = self.0[biome as usize];
        if destination == NO_ROW { NO_RULE_EDGE } else { destination as usize }
    }
}

fn biome_value(condition: &Cond, biome: BuiltinBiome) -> Option<bool> {
    match condition {
        Cond::BiomeIs { set, .. } => Some(set.contains_builtin(biome)),
        Cond::Not(inner) => biome_value(inner, biome).map(|value| !value),
        _ => None,
    }
}

fn biome_edges(node: &CompiledRuleNode) -> Option<(usize, usize, usize)> {
    match node {
        CompiledRuleNode::Condition { condition, if_true, if_false } => {
            Some((*condition, *if_true, *if_false))
        }
        _ => None,
    }
}

impl BiomeResidual {
    pub(super) fn compile(nodes: &[CompiledRuleNode], conditions: &[Cond]) -> Option<Self> {
        Self::compile_with(nodes, |index, biome| biome_value(&conditions[index], biome))
    }

    pub(super) fn compile_with(
        nodes: &[CompiledRuleNode],
        value: impl Fn(usize, BuiltinBiome) -> Option<bool>,
    ) -> Option<Self> {
        let mut rows = vec![NO_ROW; nodes.len()];
        let mut destinations = Vec::new();
        for (pc, node) in nodes.iter().enumerate() {
            let Some((condition, _, _)) = biome_edges(node) else { continue; };
            if value(condition, BuiltinBiome::Plains).is_none() {
                continue;
            }
            rows[pc] = u32::try_from(destinations.len()).expect("surface biome rows fit u32");
            for biome in BuiltinBiome::all() {
                let mut destination = pc;
                while destination != NO_RULE_EDGE {
                    let Some((condition, if_true, if_false)) = biome_edges(&nodes[destination])
                    else { break; };
                    let Some(value) = value(condition, biome) else { break; };
                    destination = if value { if_true } else { if_false };
                }
                destinations.push(if destination == NO_RULE_EDGE {
                    NO_ROW
                } else {
                    u32::try_from(destination).expect("surface rule nodes fit u32")
                });
            }
        }
        (!destinations.is_empty()).then_some(Self { rows, destinations })
    }

    #[inline]
    pub(super) fn row(&self, pc: usize) -> Option<BiomeRow<'_>> {
        let offset = self.rows[pc];
        (offset != NO_ROW).then(|| {
            let offset = offset as usize;
            BiomeRow(&self.destinations[offset..offset + BuiltinBiome::COUNT as usize])
        })
    }

    #[cfg(test)]
    pub(super) fn storage_words(&self) -> (usize, usize) {
        (self.rows.len(), self.destinations.len())
    }

    #[cfg(test)]
    pub(super) fn replace_biome_for_test(&mut self, biome: BuiltinBiome, stale: BuiltinBiome) {
        for row in self.destinations.chunks_exact_mut(BuiltinBiome::COUNT as usize) {
            row[biome as usize] = row[stale as usize];
        }
    }
}
