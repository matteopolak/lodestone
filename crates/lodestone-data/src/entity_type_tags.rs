//! The vanilla entity-type tags, nested references already expanded.
//!
//! Derived from the reference data pack's `tags/entity_type` files for the
//! release in `mc-version`; regenerate by re-expanding those files when the
//! version is bumped. Datapack-defined tags are not represented.

/// `(tag id, member entity-type ids)`, sorted by tag id.
static TAGS: &[(&str, &[&str])] = &[
    ("minecraft:accepts_iron_golem_gift", &["minecraft:copper_golem"]),
    ("minecraft:aquatic", &["minecraft:axolotl", "minecraft:cod", "minecraft:dolphin", "minecraft:elder_guardian", "minecraft:glow_squid", "minecraft:guardian", "minecraft:nautilus", "minecraft:pufferfish", "minecraft:salmon", "minecraft:squid", "minecraft:tadpole", "minecraft:tropical_fish", "minecraft:turtle", "minecraft:zombie_nautilus"]),
    ("minecraft:arrows", &["minecraft:arrow", "minecraft:spectral_arrow"]),
    ("minecraft:arthropod", &["minecraft:bee", "minecraft:cave_spider", "minecraft:endermite", "minecraft:silverfish", "minecraft:spider"]),
    ("minecraft:axolotl_always_hostiles", &["minecraft:drowned", "minecraft:elder_guardian", "minecraft:guardian"]),
    ("minecraft:axolotl_hunt_targets", &["minecraft:cod", "minecraft:glow_squid", "minecraft:pufferfish", "minecraft:salmon", "minecraft:squid", "minecraft:tadpole", "minecraft:tropical_fish"]),
    ("minecraft:beehive_inhabitors", &["minecraft:bee"]),
    ("minecraft:boat", &["minecraft:acacia_boat", "minecraft:bamboo_raft", "minecraft:birch_boat", "minecraft:cherry_boat", "minecraft:dark_oak_boat", "minecraft:jungle_boat", "minecraft:mangrove_boat", "minecraft:oak_boat", "minecraft:pale_oak_boat", "minecraft:poplar_boat", "minecraft:spruce_boat"]),
    ("minecraft:burn_in_daylight", &["minecraft:bogged", "minecraft:drowned", "minecraft:phantom", "minecraft:skeleton", "minecraft:stray", "minecraft:wither_skeleton", "minecraft:zombie", "minecraft:zombie_horse", "minecraft:zombie_nautilus", "minecraft:zombie_villager"]),
    ("minecraft:can_breathe_under_water", &["minecraft:armor_stand", "minecraft:axolotl", "minecraft:bogged", "minecraft:camel_husk", "minecraft:cod", "minecraft:copper_golem", "minecraft:drowned", "minecraft:elder_guardian", "minecraft:frog", "minecraft:glow_squid", "minecraft:guardian", "minecraft:husk", "minecraft:nautilus", "minecraft:parched", "minecraft:phantom", "minecraft:pufferfish", "minecraft:salmon", "minecraft:skeleton", "minecraft:skeleton_horse", "minecraft:squid", "minecraft:stray", "minecraft:tadpole", "minecraft:tropical_fish", "minecraft:turtle", "minecraft:wither", "minecraft:wither_skeleton", "minecraft:zoglin", "minecraft:zombie", "minecraft:zombie_horse", "minecraft:zombie_nautilus", "minecraft:zombie_villager", "minecraft:zombified_piglin"]),
    ("minecraft:can_equip_harness", &["minecraft:happy_ghast"]),
    ("minecraft:can_equip_saddle", &["minecraft:camel", "minecraft:camel_husk", "minecraft:donkey", "minecraft:horse", "minecraft:mule", "minecraft:nautilus", "minecraft:pig", "minecraft:skeleton_horse", "minecraft:strider", "minecraft:zombie_horse", "minecraft:zombie_nautilus"]),
    ("minecraft:can_float_while_ridden", &["minecraft:camel", "minecraft:camel_husk", "minecraft:donkey", "minecraft:horse", "minecraft:mule", "minecraft:zombie_horse"]),
    ("minecraft:can_turn_in_boats", &["minecraft:breeze"]),
    ("minecraft:can_wear_horse_armor", &["minecraft:horse", "minecraft:zombie_horse"]),
    ("minecraft:can_wear_nautilus_armor", &["minecraft:nautilus", "minecraft:zombie_nautilus"]),
    ("minecraft:candidate_for_iron_golem_gift", &["minecraft:copper_golem", "minecraft:villager"]),
    ("minecraft:cannot_be_age_locked", &["minecraft:skeleton_horse", "minecraft:villager", "minecraft:zombie_horse"]),
    ("minecraft:cannot_be_dismounted_by_item_usage", &["minecraft:interaction"]),
    ("minecraft:cannot_be_pushed_onto_boats", &["minecraft:cod", "minecraft:creaking", "minecraft:dolphin", "minecraft:elder_guardian", "minecraft:glow_squid", "minecraft:nautilus", "minecraft:player", "minecraft:pufferfish", "minecraft:salmon", "minecraft:squid", "minecraft:sulfur_cube", "minecraft:tadpole", "minecraft:tropical_fish", "minecraft:zombie_nautilus"]),
    ("minecraft:deflects_projectiles", &["minecraft:breeze"]),
    ("minecraft:dismounts_underwater", &["minecraft:camel", "minecraft:chicken", "minecraft:donkey", "minecraft:happy_ghast", "minecraft:horse", "minecraft:llama", "minecraft:mule", "minecraft:pig", "minecraft:ravager", "minecraft:spider", "minecraft:strider", "minecraft:trader_llama", "minecraft:zombie_horse"]),
    ("minecraft:fall_damage_immune", &["minecraft:allay", "minecraft:bat", "minecraft:bee", "minecraft:blaze", "minecraft:breeze", "minecraft:cat", "minecraft:chicken", "minecraft:copper_golem", "minecraft:ghast", "minecraft:happy_ghast", "minecraft:iron_golem", "minecraft:magma_cube", "minecraft:ocelot", "minecraft:parrot", "minecraft:phantom", "minecraft:shulker", "minecraft:snow_golem", "minecraft:wither"]),
    ("minecraft:followable_friendly_mobs", &["minecraft:armadillo", "minecraft:bee", "minecraft:camel", "minecraft:cat", "minecraft:chicken", "minecraft:cow", "minecraft:donkey", "minecraft:fox", "minecraft:goat", "minecraft:happy_ghast", "minecraft:horse", "minecraft:llama", "minecraft:mule", "minecraft:ocelot", "minecraft:panda", "minecraft:parrot", "minecraft:pig", "minecraft:polar_bear", "minecraft:rabbit", "minecraft:sheep", "minecraft:skeleton_horse", "minecraft:sniffer", "minecraft:strider", "minecraft:villager", "minecraft:wolf"]),
    ("minecraft:freeze_hurts_extra_types", &["minecraft:blaze", "minecraft:magma_cube", "minecraft:strider"]),
    ("minecraft:freeze_immune_entity_types", &["minecraft:polar_bear", "minecraft:snow_golem", "minecraft:stray", "minecraft:wither"]),
    ("minecraft:frog_food", &["minecraft:magma_cube", "minecraft:slime"]),
    ("minecraft:ignores_poison_and_regen", &["minecraft:bogged", "minecraft:camel_husk", "minecraft:drowned", "minecraft:husk", "minecraft:parched", "minecraft:phantom", "minecraft:skeleton", "minecraft:skeleton_horse", "minecraft:stray", "minecraft:wither", "minecraft:wither_skeleton", "minecraft:zoglin", "minecraft:zombie", "minecraft:zombie_horse", "minecraft:zombie_nautilus", "minecraft:zombie_villager", "minecraft:zombified_piglin"]),
    ("minecraft:illager", &["minecraft:evoker", "minecraft:illusioner", "minecraft:pillager", "minecraft:vindicator"]),
    ("minecraft:illager_friends", &["minecraft:evoker", "minecraft:illusioner", "minecraft:pillager", "minecraft:vindicator"]),
    ("minecraft:immune_to_infested", &["minecraft:silverfish"]),
    ("minecraft:immune_to_oozing", &["minecraft:slime"]),
    ("minecraft:impact_projectiles", &["minecraft:arrow", "minecraft:breeze_wind_charge", "minecraft:dragon_fireball", "minecraft:egg", "minecraft:fireball", "minecraft:firework_rocket", "minecraft:small_fireball", "minecraft:snowball", "minecraft:spectral_arrow", "minecraft:trident", "minecraft:wind_charge", "minecraft:wither_skull"]),
    ("minecraft:inverted_healing_and_harm", &["minecraft:bogged", "minecraft:camel_husk", "minecraft:drowned", "minecraft:husk", "minecraft:parched", "minecraft:phantom", "minecraft:skeleton", "minecraft:skeleton_horse", "minecraft:stray", "minecraft:wither", "minecraft:wither_skeleton", "minecraft:zoglin", "minecraft:zombie", "minecraft:zombie_horse", "minecraft:zombie_nautilus", "minecraft:zombie_villager", "minecraft:zombified_piglin"]),
    ("minecraft:nautilus_hostiles", &["minecraft:pufferfish"]),
    ("minecraft:no_anger_from_wind_charge", &["minecraft:bogged", "minecraft:breeze", "minecraft:cave_spider", "minecraft:husk", "minecraft:skeleton", "minecraft:slime", "minecraft:spider", "minecraft:stray", "minecraft:zombie"]),
    ("minecraft:non_controlling_rider", &["minecraft:magma_cube", "minecraft:slime", "minecraft:sulfur_cube"]),
    ("minecraft:not_affected_by_geysers", &["minecraft:ender_dragon"]),
    ("minecraft:not_scary_for_pufferfish", &["minecraft:cod", "minecraft:dolphin", "minecraft:elder_guardian", "minecraft:glow_squid", "minecraft:guardian", "minecraft:nautilus", "minecraft:pufferfish", "minecraft:salmon", "minecraft:squid", "minecraft:sulfur_cube", "minecraft:tadpole", "minecraft:tropical_fish", "minecraft:turtle", "minecraft:zombie_nautilus"]),
    ("minecraft:powder_snow_walkable_mobs", &["minecraft:endermite", "minecraft:fox", "minecraft:rabbit", "minecraft:silverfish"]),
    ("minecraft:raiders", &["minecraft:evoker", "minecraft:illusioner", "minecraft:pillager", "minecraft:ravager", "minecraft:vindicator", "minecraft:witch"]),
    ("minecraft:redirectable_projectile", &["minecraft:breeze_wind_charge", "minecraft:fireball", "minecraft:wind_charge"]),
    ("minecraft:sensitive_to_bane_of_arthropods", &["minecraft:bee", "minecraft:cave_spider", "minecraft:endermite", "minecraft:silverfish", "minecraft:spider"]),
    ("minecraft:sensitive_to_impaling", &["minecraft:axolotl", "minecraft:cod", "minecraft:dolphin", "minecraft:elder_guardian", "minecraft:glow_squid", "minecraft:guardian", "minecraft:nautilus", "minecraft:pufferfish", "minecraft:salmon", "minecraft:squid", "minecraft:tadpole", "minecraft:tropical_fish", "minecraft:turtle", "minecraft:zombie_nautilus"]),
    ("minecraft:sensitive_to_smite", &["minecraft:bogged", "minecraft:camel_husk", "minecraft:drowned", "minecraft:husk", "minecraft:parched", "minecraft:phantom", "minecraft:skeleton", "minecraft:skeleton_horse", "minecraft:stray", "minecraft:wither", "minecraft:wither_skeleton", "minecraft:zoglin", "minecraft:zombie", "minecraft:zombie_horse", "minecraft:zombie_nautilus", "minecraft:zombie_villager", "minecraft:zombified_piglin"]),
    ("minecraft:skeletons", &["minecraft:bogged", "minecraft:parched", "minecraft:skeleton", "minecraft:skeleton_horse", "minecraft:stray", "minecraft:wither_skeleton"]),
    ("minecraft:undead", &["minecraft:bogged", "minecraft:camel_husk", "minecraft:drowned", "minecraft:husk", "minecraft:parched", "minecraft:phantom", "minecraft:skeleton", "minecraft:skeleton_horse", "minecraft:stray", "minecraft:wither", "minecraft:wither_skeleton", "minecraft:zoglin", "minecraft:zombie", "minecraft:zombie_horse", "minecraft:zombie_nautilus", "minecraft:zombie_villager", "minecraft:zombified_piglin"]),
    ("minecraft:wither_friends", &["minecraft:bogged", "minecraft:camel_husk", "minecraft:drowned", "minecraft:husk", "minecraft:parched", "minecraft:phantom", "minecraft:skeleton", "minecraft:skeleton_horse", "minecraft:stray", "minecraft:wither", "minecraft:wither_skeleton", "minecraft:zoglin", "minecraft:zombie", "minecraft:zombie_horse", "minecraft:zombie_nautilus", "minecraft:zombie_villager", "minecraft:zombified_piglin"]),
    ("minecraft:zombies", &["minecraft:camel_husk", "minecraft:drowned", "minecraft:husk", "minecraft:zoglin", "minecraft:zombie", "minecraft:zombie_horse", "minecraft:zombie_nautilus", "minecraft:zombie_villager", "minecraft:zombified_piglin"]),
];

/// The entity types in `tag` (`minecraft:zombies`), or `None` for an unknown tag.
#[must_use]
pub fn entity_type_tag_members(tag: &str) -> Option<&'static [&'static str]> {
    TAGS.binary_search_by_key(&tag, |(name, _)| name).ok().map(|i| TAGS[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every member is a real entity type, so a stale table fails here rather than
    /// silently matching nothing.
    #[test]
    fn every_member_is_a_known_entity_type() {
        for (tag, members) in TAGS {
            for member in *members {
                assert!(
                    crate::entity_types::entity_type_id(member).is_some(),
                    "{tag} lists unknown entity type {member}"
                );
            }
        }
    }

    #[test]
    fn lookup_finds_a_tag_and_refuses_an_unknown_one() {
        assert!(entity_type_tag_members("minecraft:zombies").unwrap().contains(&"minecraft:husk"));
        assert!(entity_type_tag_members("minecraft:no_such_tag").is_none());
    }
}
