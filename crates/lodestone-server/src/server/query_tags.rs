//! NBT query results for the data-get style commands: the tag a block entity or entity answers with, gated by permission level.

use super::*;

/// The outer option denies unauthorized requests without a response; the inner
/// option reports whether the current dimension contains a block entity.
pub(super) fn block_entity_query_tag(
    entities: &BlockEntityHandle,
    permission_level: u8,
    pos: BlockPos,
) -> Option<Option<lodestone_core::Nbt>> {
    if permission_level < COMMANDS_GAMEMASTER_LEVEL {
        return None;
    }
    Some(entities.with(|registry| {
        registry.get(pos).map(|entity| {
            let mut tag = crate::chunk_nbt::block_entity_to_nbt(pos, entity);
            if let lodestone_core::Nbt::Compound(fields) = &mut tag {
                fields.retain(|(key, _)| !matches!(key.as_str(), "id" | "x" | "y" | "z" | "keepPacked"));
            }
            tag
        })
    }))
}

/// Native inspection shares the save record's modeled fields. Unsupported
/// entity kinds and unknown ids receive no reply, rather than a false empty tag.
pub(super) fn entity_query_tag(mobs: &MobHandle, permission_level: u8, entity_id: i32) -> Option<lodestone_core::Nbt> {
    if permission_level < COMMANDS_GAMEMASTER_LEVEL {
        return None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        mobs.with(|sim| {
            let uuid = sim.snapshots().into_iter().find(|entity| entity.id == entity_id)?.uuid;
            let saved = sim.saved_entities().into_iter().find(|entity| entity.uuid == uuid)?;
            let mut tag = saved.to_nbt();
            if let lodestone_core::Nbt::Compound(fields) = &mut tag {
                fields.retain(|(key, _)| key != "id");
            }
            Some(tag)
        })
    }
    #[cfg(target_arch = "wasm32")]
    {
        // The save-record serializer currently belongs to native persistence.
        let _ = (mobs, entity_id);
        None
    }
}

#[cfg(test)]
mod block_entity_query_tests {
    use super::*;
    use lodestone_core::Nbt;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn entity_query_selects_the_requested_live_mob_and_checks_permission() {
        let mobs = MobHandle::default();
        let entity_id = mobs.with(|sim| {
            sim.spawn_species("minecraft:cow".parse().unwrap(), Vec3::new(1.0, 64.0, 3.0))
                .set_health(19.0);
            sim.spawn_species("minecraft:zombie".parse().unwrap(), Vec3::new(-7.5, 68.0, 2.25))
                .set_health(7.25).id()
        });
        assert_eq!(entity_query_tag(&mobs, 1, entity_id), None);
        assert_eq!(entity_query_tag(&mobs, 2, i32::MAX), None);
        let Nbt::Compound(fields) = entity_query_tag(&mobs, 2, entity_id).unwrap() else {
            panic!("live mob query returns a compound")
        };
        assert!(fields.contains(&("Health".into(), Nbt::Float(7.25))));
        assert!(fields.contains(&("Pos".into(), Nbt::List {
            element_type: lodestone_core::NbtTag::Double,
            elements: vec![Nbt::Double(-7.5), Nbt::Double(68.0), Nbt::Double(2.25)],
        })));
        assert!(!fields.iter().any(|(key, _)| key == "id"));
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn entity_query_preserves_item_identity_and_lifecycle() {
        let mobs = MobHandle::default();
        let entity_id = mobs.with(|sim| sim.spawn_item(
            "minecraft:diamond".parse().unwrap(), Vec3::new(3.0, 65.0, 9.0),
            Vec3::new(0.0, -0.25, 0.0),
            lodestone_entity::item_entity::ItemLifecycle { age: 73, pickup_delay: 6, count: 5, max_stack_size: 64 },
        ));
        let Nbt::Compound(fields) = entity_query_tag(&mobs, 2, entity_id).unwrap() else {
            panic!("dropped item query returns a compound")
        };
        assert!(fields.contains(&("Item".into(), Nbt::Compound(vec![
            ("id".into(), Nbt::String("minecraft:diamond".into())),
            ("count".into(), Nbt::Int(5)),
        ]))));
        assert!(fields.contains(&("Age".into(), Nbt::Short(73))));
        assert!(fields.contains(&("PickupDelay".into(), Nbt::Short(6))));
        assert!(!fields.iter().any(|(key, _)| key == "id"));
        mobs.with(|sim| { sim.remove_item(entity_id); });
        assert_eq!(entity_query_tag(&mobs, 2, entity_id), None);
    }

    #[test]
    fn block_entity_query_checks_permission_and_strips_only_metadata() {
        let entities = BlockEntityHandle::new();
        let pos = BlockPos::new(-3, -17, 5);
        entities.with(|registry| registry.insert(pos, crate::block_entities::BlockEntity::Opaque {
            id: "minecraft:chest".into(),
            nbt: Nbt::Compound(vec![
                ("id".into(), Nbt::String("minecraft:chest".into())),
                ("x".into(), Nbt::Int(-3)),
                ("y".into(), Nbt::Int(-17)),
                ("z".into(), Nbt::Int(5)),
                ("CustomName".into(), Nbt::String("Supplies".into())),
            ]),
        }));
        assert_eq!(block_entity_query_tag(&entities, 1, pos), None);
        assert_eq!(block_entity_query_tag(&entities, 2, pos), Some(Some(Nbt::Compound(vec![
            ("CustomName".into(), Nbt::String("Supplies".into())),
        ]))));
        assert_eq!(block_entity_query_tag(&entities, 2, BlockPos::new(9, 8, 7)), Some(None));
        entities.with(|registry| {
            let crate::block_entities::BlockEntity::Opaque { nbt: Nbt::Compound(fields), .. } =
                registry.get(pos).unwrap() else { panic!("opaque compound retained") };
            assert_eq!(fields.len(), 5, "query must not mutate saved metadata");
        });
    }

    #[test]
    fn block_entity_query_serializes_the_live_container() {
        let entities = BlockEntityHandle::new();
        let pos = BlockPos::new(7, 64, -9);
        entities.with(|registry| registry.insert(pos, crate::block_entities::BlockEntity::Container {
            id: "minecraft:chest".into(),
            slots: vec![Some(ItemStack::new("minecraft:apple".parse().unwrap(), 5))],
        }));
        assert_eq!(block_entity_query_tag(&entities, 2, pos), Some(Some(Nbt::Compound(vec![
            ("components".into(), Nbt::Compound(vec![])),
            ("Items".into(), Nbt::List {
                element_type: lodestone_core::NbtTag::Compound,
                elements: vec![Nbt::Compound(vec![
                    ("Slot".into(), Nbt::Byte(0)),
                    ("id".into(), Nbt::String("minecraft:apple".into())),
                    ("count".into(), Nbt::Int(5)),
                ])],
            }),
        ]))));
    }
}
