//! Per-species pathfinding-malus overrides come from the generated table.

use lodestone_entity::pathfinding::{PathType, species_malus_overrides};

fn cost(species: &str, kind: PathType) -> Option<f32> {
    species_malus_overrides(species).iter().find(|(k, _)| *k == kind).map(|(_, c)| *c)
}

/// Costs each species sets on itself, read off the reference's constructors
/// (the survey's list), including the ones the old hand-written table lacked.
#[test]
fn the_table_carries_the_overrides_the_hand_written_one_lacked() {
    assert_eq!(cost("fox", PathType::Damaging), Some(0.0));
    assert_eq!(cost("fox", PathType::DamagingInNeighbor), Some(0.0));
    assert_eq!(cost("goat", PathType::PowderSnow), Some(-1.0));
    assert_eq!(cost("goat", PathType::OnTopOfPowderSnow), Some(-1.0));
    assert_eq!(cost("turtle", PathType::Water), Some(0.0));
    assert_eq!(cost("turtle", PathType::DoorWoodClosed), Some(-1.0));
    assert_eq!(cost("turtle", PathType::DoorOpen), Some(-1.0));
    assert_eq!(cost("frog", PathType::Water), Some(4.0));
    assert_eq!(cost("frog", PathType::Trapdoor), Some(-1.0));
    for swimmer in ["axolotl", "cod", "squid"] {
        assert_eq!(cost(swimmer, PathType::Water), Some(0.0), "{swimmer}");
    }
    assert_eq!(cost("ravager", PathType::Leaves), Some(0.0));
    assert_eq!(cost("sniffer", PathType::Water), Some(-1.0));
    assert_eq!(cost("sniffer", PathType::DamageCautious), Some(-1.0));
    assert_eq!(cost("breeze", PathType::OnTopOfTrapdoor), Some(-1.0));
    assert_eq!(cost("copper_golem", PathType::DamagingInNeighbor), Some(16.0));
}

/// An animal's inherited fire costs reach every animal subclass, and a mob
/// that overrides them wins over its parent.
#[test]
fn inherited_costs_apply_and_subclass_overrides_win() {
    assert_eq!(cost("bee", PathType::FireInNeighbor), Some(16.0));
    assert_eq!(cost("hoglin", PathType::Fire), Some(-1.0));
    assert_eq!(cost("parrot", PathType::FireInNeighbor), Some(-1.0));
    assert_eq!(cost("strider", PathType::Fire), Some(0.0));
}

#[test]
fn a_species_with_no_overrides_has_none() {
    assert!(species_malus_overrides("zombie").is_empty());
    assert!(species_malus_overrides("not_a_species").is_empty());
}
