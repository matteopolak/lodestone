//! Configured placement of a weighted structure-template entry.

use std::sync::Arc;

use serde_json::Value;

use crate::density::Resolver;
use crate::feature::BlockPos;
use crate::rng::RandomSource;
use crate::structure::template::{Rotation, StructureTemplate};

use super::fossil;
use super::grid::VegGrid;

#[derive(Clone, Debug)]
pub struct TemplateCfg {
    entries: Vec<TemplateEntry>,
    total_weight: i32,
}

#[derive(Clone, Debug)]
struct TemplateEntry {
    template: Arc<StructureTemplate>,
    weight: i32,
    rotations: Vec<Rotation>,
    rotation_count: i32,
}

impl TemplateCfg {
    pub(super) fn try_parse(resolver: &dyn Resolver, value: &Value) -> Option<Self> {
        let entries = value["templates"]
            .as_array()?
            .iter()
            .map(|entry| {
                let id = entry["data"]["id"].as_str()?;
                let weight = entry["weight"].as_i64().and_then(|weight| i32::try_from(weight).ok())?;
                (weight > 0).then_some(())?;
                let rotations = entry["data"]["rotations"]
                    .as_array()
                    .map(|rotations| {
                        rotations
                            .iter()
                            .map(|rotation| match rotation.as_str()? {
                                "none" => Some(Rotation::None),
                                "clockwise_90" => Some(Rotation::Cw90),
                                "clockwise_180" => Some(Rotation::Cw180),
                                "counterclockwise_90" => Some(Rotation::Ccw90),
                                _ => None,
                            })
                            .collect::<Option<Vec<_>>>()
                    })
                    .unwrap_or_else(|| Some(vec![
                        Rotation::None,
                        Rotation::Cw90,
                        Rotation::Cw180,
                        Rotation::Ccw90,
                    ]))?;
                (!rotations.is_empty()).then_some(())?;
                let rotation_count = i32::try_from(rotations.len()).ok()?;
                let template = StructureTemplate::parse(&resolver.structure_template(id)?).ok()?;
                template.size().iter().all(|size| *size > 0).then_some(TemplateEntry {
                    template: Arc::new(template),
                    weight,
                    rotations,
                    rotation_count,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        let total_weight = entries
            .iter()
            .try_fold(0i32, |total, entry| total.checked_add(entry.weight))?;
        (total_weight > 0).then_some(Self { entries, total_weight })
    }

    pub(super) fn place<R: RandomSource>(
        &self,
        random: &mut R,
        origin: BlockPos,
        grid: &mut VegGrid,
    ) -> bool {
        let mut roll = random.next_int_bounded(self.total_weight);
        let entry = self
            .entries
            .iter()
            .find(|entry| {
                roll -= entry.weight;
                roll < 0
            })
            .expect("validated total weights must select an entry");
        let rotation = entry.rotations[random.next_int_bounded(entry.rotation_count) as usize];
        let target = anchor(origin, entry.template.size(), rotation);
        fossil::place_template(&entry.template, &[], target, rotation, grid);
        true
    }
}

fn anchor(origin: BlockPos, size: [i32; 3], rotation: Rotation) -> [i32; 3] {
    let x = size[0] / 2;
    let z = size[2] / 2;
    let (dx, dz) = match rotation {
        Rotation::None => (-x, -z),
        Rotation::Cw90 => (z, -x),
        Rotation::Cw180 => (x, z),
        Rotation::Ccw90 => (-z, x),
    };
    [origin.x + dx, origin.y, origin.z + dz]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::{RandomSource, XoroshiroPositionalFactory};
    use crate::structure::template::BlockState;
    use lodestone_data::block::Block;

    struct ScriptedRandom {
        draws: Vec<i32>,
        cursor: usize,
    }

    impl ScriptedRandom {
        fn new(draws: &[i32]) -> Self {
            Self { draws: draws.to_vec(), cursor: 0 }
        }
    }

    impl RandomSource for ScriptedRandom {
        type Positional = XoroshiroPositionalFactory;

        fn fork_positional(&mut self) -> Self::Positional { panic!("fixture does not fork") }
        fn set_seed(&mut self, _: i64) { panic!("fixture does not reseed") }
        fn next_bits(&mut self, _: u32) -> i32 { panic!("fixture does not draw bits") }
        fn next_int(&mut self) -> i32 { panic!("fixture does not draw unbounded ints") }
        fn next_int_bounded(&mut self, bound: i32) -> i32 {
            let draw = self.draws[self.cursor];
            self.cursor += 1;
            assert!((0..bound).contains(&draw));
            draw
        }
        fn next_long(&mut self) -> i64 { panic!("fixture does not draw longs") }
        fn next_bool(&mut self) -> bool { panic!("fixture does not draw bools") }
        fn next_float(&mut self) -> f32 { panic!("fixture does not draw floats") }
        fn next_double(&mut self) -> f64 { panic!("fixture does not draw doubles") }
        fn next_gaussian(&mut self) -> f64 { panic!("fixture does not draw gaussian") }
        fn consume_count(&mut self, _: u32) { panic!("fixture does not consume draws") }
    }

    #[test]
    fn weighted_choice_precedes_rotation_and_rotates_half_size_anchor() {
        let tuff = Arc::new(StructureTemplate::from_blocks(
            [6, 1, 4],
            vec![BlockState::of(Block::Tuff)],
            vec![([0, 0, 0], 0)],
        ));
        let deepslate = Arc::new(StructureTemplate::from_blocks(
            [6, 1, 4],
            vec![BlockState::of(Block::Deepslate)],
            vec![([0, 0, 0], 0)],
        ));
        let feature = TemplateCfg {
            entries: vec![
                TemplateEntry {
                    template: tuff,
                    weight: 1,
                    rotations: vec![Rotation::None],
                    rotation_count: 1,
                },
                TemplateEntry {
                    template: deepslate,
                    weight: 1,
                    rotations: vec![Rotation::Ccw90],
                    rotation_count: 1,
                },
            ],
            total_weight: 2,
        };
        let mut random = ScriptedRandom::new(&[1, 0]);
        let mut grid = VegGrid::with_footprint(0, 32, 100, 200, -16, 16);

        assert!(feature.place(&mut random, BlockPos { x: 100, y: 20, z: 200 }, &mut grid));
        assert_eq!(grid.get_id(98, 20, 203), Block::Deepslate.default_state());
        assert_eq!(grid.dirty_len(), 1);
    }
}
