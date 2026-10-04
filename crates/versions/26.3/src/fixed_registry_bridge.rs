//! Fixed-registry callbacks injected into the shared packet codec.

use lodestone_v26_2::dialect::{FixedRegistryKind, FixedRegistryMappings};

use crate::fixed_registries::{CanonicalFixedId, FixedRegistry, WireFixedId, WireVersion};

fn registry(kind: FixedRegistryKind) -> FixedRegistry {
    match kind {
        FixedRegistryKind::BlockEntity => FixedRegistry::BlockEntityType,
        FixedRegistryKind::Entity => FixedRegistry::EntityType,
        FixedRegistryKind::Particle => FixedRegistry::ParticleType,
        FixedRegistryKind::Sound => FixedRegistry::SoundEvent,
        FixedRegistryKind::DataComponent => FixedRegistry::DataComponentType,
        FixedRegistryKind::Attribute => FixedRegistry::Attribute,
        FixedRegistryKind::Menu => FixedRegistry::Menu,
        FixedRegistryKind::CustomStat => FixedRegistry::CustomStat,
        FixedRegistryKind::CommandParser => FixedRegistry::CommandArgumentType,
        FixedRegistryKind::MapDecoration => FixedRegistry::MapDecorationType,
        FixedRegistryKind::PositionSource => FixedRegistry::PositionSourceType,
        FixedRegistryKind::DebugSubscription => FixedRegistry::DebugSubscription,
        FixedRegistryKind::VillagerType => FixedRegistry::VillagerType,
        FixedRegistryKind::VillagerProfession => FixedRegistry::VillagerProfession,
    }
}

fn decode_version(version: WireVersion, kind: FixedRegistryKind, raw: i32) -> Option<i32> {
    let wire = WireFixedId::new(version, registry(kind), raw)?;
    i32::try_from(wire.to_canonical().raw()).ok()
}

fn encode_version(version: WireVersion, kind: FixedRegistryKind, raw: i32) -> Option<i32> {
    let canonical = CanonicalFixedId::new(registry(kind), u32::try_from(raw).ok()?)?;
    i32::try_from(canonical.to_wire(version)?.raw()).ok()
}

fn decode_latest(kind: FixedRegistryKind, raw: i32) -> Option<i32> {
    decode_version(WireVersion::V26_3, kind, raw)
}

fn encode_latest(kind: FixedRegistryKind, raw: i32) -> Option<i32> {
    encode_version(WireVersion::V26_3, kind, raw)
}

fn decode_base(kind: FixedRegistryKind, raw: i32) -> Option<i32> {
    decode_version(WireVersion::V26_2, kind, raw)
}

fn encode_base(kind: FixedRegistryKind, raw: i32) -> Option<i32> {
    encode_version(WireVersion::V26_2, kind, raw)
}

fn name(kind: FixedRegistryKind, canonical: i32) -> Option<&'static str> {
    CanonicalFixedId::new(registry(kind), u32::try_from(canonical).ok()?).map(CanonicalFixedId::name)
}

pub static V26_3_FIXED_REGISTRIES: FixedRegistryMappings = FixedRegistryMappings {
    decode: decode_latest,
    encode: encode_latest,
    name,
};

pub static V26_2_FIXED_REGISTRIES: FixedRegistryMappings = FixedRegistryMappings {
    decode: decode_base,
    encode: encode_base,
    name,
};

#[cfg(test)]
mod tests {
    use lodestone_data::particle_types::{ParticleTypeId, is_simple_particle_type};
    use lodestone_v26_2::dialect::FixedRegistryKind;

    use super::{V26_2_FIXED_REGISTRIES, V26_3_FIXED_REGISTRIES};

    #[test]
    fn literal_external_ids_reach_the_injected_bridge() {
        let maps = &V26_3_FIXED_REGISTRIES;
        assert_eq!((maps.decode)(FixedRegistryKind::Entity, 34), Some(33));
        assert_eq!((maps.encode)(FixedRegistryKind::Entity, 33), Some(34));
        assert_eq!((maps.decode)(FixedRegistryKind::Entity, 33), Some(158));
        assert_eq!((maps.name)(FixedRegistryKind::Entity, 158), Some("minecraft:cushion"));
        assert_eq!((maps.decode)(FixedRegistryKind::Sound, 804), Some(800));
        assert_eq!((maps.encode)(FixedRegistryKind::Sound, 800), Some(804));
        assert_eq!((maps.decode)(FixedRegistryKind::BlockEntity, 1), Some(1));
        assert_eq!((maps.decode)(FixedRegistryKind::Attribute, 26), Some(26));
        assert_eq!((maps.decode)(FixedRegistryKind::Menu, 0), Some(0));
    }

    #[test]
    fn component_alias_and_negative_boundaries_remain_explicit() {
        let maps = &V26_3_FIXED_REGISTRIES;
        assert_eq!((maps.decode)(FixedRegistryKind::DataComponent, 40), Some(111));
        assert_eq!((maps.encode)(FixedRegistryKind::DataComponent, 40), None);
        assert_eq!((maps.encode)(FixedRegistryKind::DataComponent, 111), Some(40));
        assert_eq!((maps.name)(FixedRegistryKind::DataComponent, 40), Some("minecraft:swing_animation"));
        assert_eq!((maps.name)(FixedRegistryKind::DataComponent, 111), Some("minecraft:attack_animation"));
        for raw in [-1, i32::MAX] {
            assert_eq!((maps.decode)(FixedRegistryKind::DataComponent, raw), None);
            assert_eq!((maps.encode)(FixedRegistryKind::DataComponent, raw), None);
            assert_eq!((maps.name)(FixedRegistryKind::DataComponent, raw), None);
        }
    }

    #[test]
    fn name_callbacks_cover_new_maps_stats_and_command_parsers() {
        let maps = &V26_3_FIXED_REGISTRIES;
        assert_eq!((maps.decode)(FixedRegistryKind::MapDecoration, 35), Some(35));
        assert_eq!((maps.name)(FixedRegistryKind::MapDecoration, 35), Some("minecraft:abandoned_camp"));
        assert_eq!((maps.decode)(FixedRegistryKind::CustomStat, 61), Some(77));
        assert_eq!((maps.name)(FixedRegistryKind::CustomStat, 77), Some("minecraft:sleep_in_straw_bed"));
        assert_eq!((maps.decode)(FixedRegistryKind::CommandParser, 55), Some(57));
        assert_eq!((maps.name)(FixedRegistryKind::CommandParser, 57), Some("minecraft:context_float_provider"));
        assert_eq!((maps.decode)(FixedRegistryKind::CommandParser, 61), Some(56));
        assert_eq!((maps.name)(FixedRegistryKind::CommandParser, 56), Some("minecraft:uuid"));
    }

    #[test]
    fn new_leaf_particles_resolve_to_the_total_canonical_flag_table() {
        let maps = &V26_3_FIXED_REGISTRIES;
        for (wire, canonical, expected_name) in [
            (43, 125, "minecraft:red_poplar_leaves"),
            (44, 126, "minecraft:orange_poplar_leaves"),
            (45, 127, "minecraft:yellow_poplar_leaves"),
        ] {
            assert_eq!((maps.decode)(FixedRegistryKind::Particle, wire), Some(canonical));
            assert_eq!((maps.name)(FixedRegistryKind::Particle, canonical), Some(expected_name));
            let id = ParticleTypeId::new(canonical).expect("canonical particle is in the generated census");
            assert!(is_simple_particle_type(id));
        }
        assert_eq!((maps.decode)(FixedRegistryKind::Particle, 46), Some(43));
        let tinted = ParticleTypeId::new(43).unwrap();
        assert!(!is_simple_particle_type(tinted));
    }

    #[test]
    fn old_wire_bounds_remain_independent_of_the_union_data_counts() {
        let maps = &V26_2_FIXED_REGISTRIES;
        assert_eq!((maps.decode)(FixedRegistryKind::DataComponent, 110), Some(110));
        assert_eq!((maps.decode)(FixedRegistryKind::DataComponent, 111), None);
        assert_eq!((maps.encode)(FixedRegistryKind::DataComponent, 111), None);
        assert_eq!((maps.decode)(FixedRegistryKind::Particle, 124), Some(124));
        assert_eq!((maps.decode)(FixedRegistryKind::Particle, 125), None);
        assert_eq!((maps.encode)(FixedRegistryKind::Particle, 125), None);
        assert_eq!((maps.decode)(FixedRegistryKind::Entity, 158), None);
        assert_eq!((maps.encode)(FixedRegistryKind::Entity, 158), None);
        assert_eq!((maps.decode)(FixedRegistryKind::MapDecoration, 35), None);
        assert_eq!((maps.encode)(FixedRegistryKind::CustomStat, 77), None);
        assert_eq!((maps.decode)(FixedRegistryKind::PositionSource, 2), None);
        assert_eq!((maps.decode)(FixedRegistryKind::DebugSubscription, 16), None);
    }

    #[test]
    fn villager_report_witnesses_and_bounds_agree_for_both_releases() {
        for maps in [&V26_2_FIXED_REGISTRIES, &V26_3_FIXED_REGISTRIES] {
            for (kind, raw, expected_name) in [
                (FixedRegistryKind::VillagerType, 0, "minecraft:desert"),
                (FixedRegistryKind::VillagerType, 6, "minecraft:taiga"),
                (FixedRegistryKind::VillagerProfession, 5, "minecraft:farmer"),
                (FixedRegistryKind::VillagerProfession, 11, "minecraft:nitwit"),
                (FixedRegistryKind::VillagerProfession, 14, "minecraft:weaponsmith"),
            ] {
                assert_eq!((maps.decode)(kind, raw), Some(raw));
                assert_eq!((maps.encode)(kind, raw), Some(raw));
                assert_eq!((maps.name)(kind, raw), Some(expected_name));
            }
            for (kind, bound) in [
                (FixedRegistryKind::VillagerType, 7),
                (FixedRegistryKind::VillagerProfession, 15),
            ] {
                for raw in [-1, bound] {
                    assert_eq!((maps.decode)(kind, raw), None);
                    assert_eq!((maps.encode)(kind, raw), None);
                    assert_eq!((maps.name)(kind, raw), None);
                }
            }
        }
    }
}
