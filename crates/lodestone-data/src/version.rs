use lodestone_model::BlockAabb;

use crate::block::Block;
use crate::block_blast::BlockBlast;
use crate::block_states::StateId;
use crate::item::Item;
use crate::{generated_behavior_versions as behavior, generated_identity_versions as identity};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum GameDataVersion {
    #[default]
    V26_2,
    V26_3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateFacts {
    pub air: bool,
    pub fluid: bool,
    pub ocean_floor: bool,
    pub motion_heightmap: bool,
    pub no_leaves_heightmap: bool,
    pub fluid_blocker: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct StatePredicates(GameDataVersion);

impl GameDataVersion {
    pub const fn block_count(self) -> u32 {
        match self {
            Self::V26_2 => identity::v26_2::WIRE_BLOCK_COUNT,
            Self::V26_3 => identity::v26_3::WIRE_BLOCK_COUNT,
        }
    }

    pub const fn state_count(self) -> u32 {
        match self {
            Self::V26_2 => identity::v26_2::WIRE_BLOCK_STATE_COUNT,
            Self::V26_3 => identity::v26_3::WIRE_BLOCK_STATE_COUNT,
        }
    }

    pub const fn item_count(self) -> u32 {
        match self {
            Self::V26_2 => identity::v26_2::WIRE_ITEM_COUNT,
            Self::V26_3 => identity::v26_3::WIRE_ITEM_COUNT,
        }
    }

    pub const fn predicates(self) -> StatePredicates {
        StatePredicates(self)
    }

    pub fn supports_state(self, state: StateId) -> bool {
        self.state_to_wire(state).is_some()
    }

    pub fn default_state(self, block: Block) -> Option<StateId> {
        let states = match self {
            Self::V26_2 => &identity::v26_2::BLOCK_DEFAULT_STATES,
            Self::V26_3 => &identity::v26_3::BLOCK_DEFAULT_STATES,
        };
        states[usize::from(block.registry_id())].and_then(StateId::new)
    }

    pub fn block_from_wire(self, raw: u32) -> Option<Block> {
        let map: &[u32] = match self {
            Self::V26_2 => &identity::v26_2::BLOCK_WIRE_TO_CANONICAL,
            Self::V26_3 => &identity::v26_3::BLOCK_WIRE_TO_CANONICAL,
        };
        map.get(raw as usize).copied()
            .and_then(|id| u16::try_from(id).ok())
            .and_then(Block::from_registry_id)
    }

    pub fn block_to_wire(self, block: Block) -> Option<u32> {
        let map = match self {
            Self::V26_2 => &identity::v26_2::BLOCK_CANONICAL_TO_WIRE,
            Self::V26_3 => &identity::v26_3::BLOCK_CANONICAL_TO_WIRE,
        };
        map[usize::from(block.registry_id())]
    }

    pub fn state_from_wire(self, raw: u32) -> Option<StateId> {
        let map: &[u32] = match self {
            Self::V26_2 => &identity::v26_2::BLOCK_STATE_WIRE_TO_CANONICAL,
            Self::V26_3 => &identity::v26_3::BLOCK_STATE_WIRE_TO_CANONICAL,
        };
        map.get(raw as usize).copied().and_then(StateId::new)
    }

    pub fn state_to_wire(self, state: StateId) -> Option<u32> {
        let map = match self {
            Self::V26_2 => &identity::v26_2::BLOCK_STATE_CANONICAL_TO_WIRE,
            Self::V26_3 => &identity::v26_3::BLOCK_STATE_CANONICAL_TO_WIRE,
        };
        map[state.index()]
    }

    pub fn item_from_wire(self, raw: u32) -> Option<Item> {
        let map: &[u32] = match self {
            Self::V26_2 => &identity::v26_2::ITEM_WIRE_TO_CANONICAL,
            Self::V26_3 => &identity::v26_3::ITEM_WIRE_TO_CANONICAL,
        };
        map.get(raw as usize).copied()
            .and_then(|id| u16::try_from(id).ok())
            .and_then(Item::from_registry_id)
    }

    pub fn item_to_wire(self, item: Item) -> Option<u32> {
        let map = match self {
            Self::V26_2 => &identity::v26_2::ITEM_CANONICAL_TO_WIRE,
            Self::V26_3 => &identity::v26_3::ITEM_CANONICAL_TO_WIRE,
        };
        map[usize::from(item.registry_id())]
    }

    pub fn item_prototype(self, item: Item) -> Option<&'static crate::item_prototypes::ItemPrototypeDef> {
        crate::item_prototypes::prototype_for_version(self, item)
    }

    pub fn movement(self, state: StateId) -> Option<lodestone_model::BlockMovement> {
        crate::movement::for_state(self, state)
    }

    pub const fn block_state_wire_bits(self) -> u32 {
        match self {
            Self::V26_2 => 15,
            Self::V26_3 => 16,
        }
    }

    pub fn outline_boxes(self, state: StateId) -> &'static [BlockAabb] {
        let shape = self.latest_override(&behavior::OUTLINE, state.raw())
            .unwrap_or(crate::generated_outline_shapes::STATE_OUTLINE[state.index()]);
        crate::generated_outline_shapes::OUTLINE_SHAPES[usize::from(shape)]
    }

    pub fn legacy_solid(self, state: StateId) -> bool {
        self.latest_override(&behavior::LEGACY_SOLID, state.raw())
            .unwrap_or_else(|| crate::block_solidity::legacy_solid(state))
    }

    pub fn blast(self, block: Block) -> BlockBlast {
        let entry = self.latest_override(&behavior::BLAST, u32::from(block.registry_id()))
            .unwrap_or(crate::generated_block_blast::ENTRY_BY_REGISTRY_ID[usize::from(block.registry_id())]);
        let (bits, ignite_odds, burn_odds, ignited_by_lava) =
            crate::generated_block_blast::ENTRIES[usize::from(entry)];
        BlockBlast {
            explosion_resistance: f32::from_bits(bits),
            ignite_odds,
            burn_odds,
            ignited_by_lava,
        }
    }

    pub fn explosion_resistance(self, state: StateId) -> Option<f32> {
        let entry = self.latest_override(&behavior::EFFECTIVE_RESISTANCE, state.raw())
            .unwrap_or(crate::generated_block_blast::STATE_RESISTANCE_ENTRY[state.index()]);
        let bits = crate::generated_block_blast::RESISTANCE_VALUES[usize::from(entry)];
        (bits != crate::generated_block_blast::EMPTY_RESISTANCE).then(|| f32::from_bits(bits))
    }

    fn latest_override<T: Copy>(self, overrides: &[(u32, T)], id: u32) -> Option<T> {
        if self == Self::V26_3 {
            overrides.binary_search_by_key(&id, |entry| entry.0)
                .ok().map(|index| overrides[index].1)
        } else {
            None
        }
    }
}

impl StatePredicates {
    pub fn facts(self, state: StateId) -> StateFacts {
        let air = matches!(state.block(), Block::Air | Block::CaveAir | Block::VoidAir);
        let fluid = crate::snow_support::has_fluid_state(state);
        match self.0 {
            GameDataVersion::V26_2 => {
                let motion = crate::block_solidity::blocks_motion(state);
                let motion_heightmap = !air && (fluid || motion);
                StateFacts {
                    air,
                    fluid,
                    ocean_floor: motion,
                    motion_heightmap,
                    no_leaves_heightmap: motion_heightmap
                        && !matches!(state.block(),
                            Block::OakLeaves | Block::SpruceLeaves | Block::BirchLeaves
                                | Block::JungleLeaves | Block::AcaciaLeaves | Block::CherryLeaves
                                | Block::DarkOakLeaves | Block::PaleOakLeaves | Block::MangroveLeaves
                                | Block::AzaleaLeaves | Block::FloweringAzaleaLeaves),
                    fluid_blocker: motion,
                }
            }
            GameDataVersion::V26_3 => StateFacts {
                air,
                fluid,
                ocean_floor: bit(&behavior::OCEAN_FLOOR, state),
                motion_heightmap: bit(&behavior::MOTION_HEIGHTMAP, state),
                no_leaves_heightmap: bit(&behavior::NO_LEAVES_HEIGHTMAP, state),
                fluid_blocker: bit(&behavior::FLUID_BLOCKER, state),
            },
        }
    }

    pub fn generic_motion_tag(self, state: StateId) -> Option<bool> {
        (self.0 == GameDataVersion::V26_3).then(|| bit(&behavior::GENERIC_MOTION, state))
    }
}

fn bit(column: &[u8], state: StateId) -> bool {
    column[state.index() / 8] & (1 << (state.raw() % 8)) != 0
}

const _: () = assert!(behavior::STATE_COUNT == crate::block_states::STATE_COUNT);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selected_wire_identity_preserves_the_canonical_prefix() {
        let old = GameDataVersion::V26_2;
        let new = GameDataVersion::V26_3;
        assert_eq!((old.block_count(), old.state_count(), old.item_count()), (1196, 32366, 1537));
        assert_eq!((new.block_count(), new.state_count(), new.item_count()), (1286, 35723, 1658));
        assert_eq!(old.state_from_wire(252).unwrap().raw(), 252);
        assert_eq!(new.state_from_wire(267).unwrap().raw(), 252);
        assert_eq!(new.state_from_wire(18402).unwrap().raw(), 35722);
        assert_eq!(new.state_to_wire(StateId::new(35722).unwrap()), Some(18402));
        assert_eq!(old.state_to_wire(StateId::new(35722).unwrap()), None);
        assert_eq!(old.state_from_wire(32366), None);
        assert_eq!(new.state_from_wire(35723), None);
    }

    #[test]
    fn heightmaps_and_fluid_blockers_are_independent_predicates() {
        let new = GameDataVersion::V26_3.predicates();
        for (id, expected) in [
            (252, (true, true, true, true, true)),
            (253, (true, true, true, true, false)),
            (5336, (false, true, false, false, false)),
            (13399, (true, true, true, true, true)),
            (35722, (true, true, true, true, true)),
        ] {
            let state = StateId::new(id).unwrap();
            let facts = new.facts(state);
            assert_eq!((new.generic_motion_tag(state).unwrap(), facts.fluid_blocker,
                facts.ocean_floor, facts.motion_heightmap, facts.no_leaves_heightmap), expected, "{id}");
        }
        let sign = StateId::new(5336).unwrap();
        assert!(GameDataVersion::V26_2.predicates().facts(sign).motion_heightmap);
        assert!(!new.facts(sign).motion_heightmap);
        assert_eq!(GameDataVersion::V26_2.predicates().generic_motion_tag(sign), None);
    }

    #[test]
    fn shared_identity_can_select_different_captured_behavior() {
        let state = StateId::new(11131).unwrap();
        assert_eq!(GameDataVersion::V26_2.outline_boxes(state)[0].max[1].to_bits(), 0x3f000000);
        assert_eq!(GameDataVersion::V26_3.outline_boxes(state)[0].max[1].to_bits(), 0x3f080000);
        for id in [9015, 9018] {
            let state = StateId::new(id).unwrap();
            assert!(!GameDataVersion::V26_2.legacy_solid(state));
            assert!(GameDataVersion::V26_3.legacy_solid(state));
        }
        for block in [Block::QuartzSlab, Block::RedSandstoneSlab, Block::CutRedSandstoneSlab] {
            assert_eq!(GameDataVersion::V26_2.blast(block).explosion_resistance.to_bits(), 0x40c00000);
            assert_eq!(GameDataVersion::V26_3.blast(block).explosion_resistance.to_bits(), 0x3f4ccccd);
        }
    }
}
