//! The `VersionAdapter::{block_outline, block_interaction, item_prototype}`
//! seams: proves the two new version-owned censuses actually reach a
//! version-free consumer **through the trait object**.
//!
//! Every adapter here is bound as `&dyn VersionAdapter` before it is called.
//! That is the whole point: `tests/outline_shapes.rs` and
//! `tests/item_prototypes.rs` already cover the concrete tables value-for-value,
//! so a test calling `V770Adapter::block_outline` directly would prove nothing
//! new. What can silently break is the *seam* — a missing `impl` override leaves
//! the trait's `None` default in place, and the shell sees "this version has no
//! outline data" while the table sits right there. That is nine-times-confirmed
//! the dominant defect shape in this repo. Modelled on `block_hardness_seam.rs`.

use lodestone_model::{EquipmentSlot, VersionAdapter};
use lodestone_data::{block_states, item_prototypes, outline_shapes};
use lodestone_v26_2::V770Adapter;

/// Binds the concrete adapter behind the trait object, so every assertion below
/// travels the same dynamic-dispatch path a version-free consumer uses after
/// `lodestone_registry::adapter_for_protocol`.
fn seam() -> Box<dyn VersionAdapter> {
    Box::new(V770Adapter::new())
}

fn first_id_named(name: &str) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} present in the block-state table"))
}

// ---------------------------------------------------------------------------
// Outline / interaction shapes
// ---------------------------------------------------------------------------

#[test]
fn outline_seam_returns_real_shapes_not_the_trait_default() {
    let adapter = seam();

    // A full cube, an empty shape and a partial shape, so a seam that returned a
    // constant of any kind fails.
    let stone = adapter
        .block_outline(first_id_named("minecraft:stone"))
        .expect("stone resolves through the trait object");
    assert_eq!(stone.len(), 1);
    assert_eq!(stone[0].min, [0.0; 3]);
    assert_eq!(stone[0].max, [1.0; 3]);

    let water = adapter
        .block_outline(first_id_named("minecraft:water"))
        .expect("water resolves through the trait object");
    assert!(
        water.is_empty(),
        "water must outline to nothing — that is what makes it untargetable"
    );

    let kelp = adapter
        .block_outline(first_id_named("minecraft:kelp"))
        .expect("kelp resolves through the trait object");
    assert_eq!(kelp.len(), 1);
    assert_eq!(kelp[0].max[1], 0.5625, "kelp outlines to 9/16 high");
}

/// The seam must *not* answer the outline question with the collision table.
/// Cobweb is the sharpest discriminator: a full-cube outline and no collision.
#[test]
fn outline_seam_is_not_the_collision_seam() {
    let adapter = seam();
    let cobweb = first_id_named("minecraft:cobweb");
    let outline = adapter.block_outline(cobweb).expect("outline resolves");
    let collision = adapter.block_collision(cobweb).expect("collision resolves");
    assert_eq!(outline.len(), 1, "cobweb outlines to a full cube");
    assert!(collision.is_empty(), "cobweb collides with nothing");
}

#[test]
fn interaction_seam_returns_real_shapes_not_the_trait_default() {
    let adapter = seam();
    let hopper = adapter
        .block_interaction(first_id_named("minecraft:hopper"))
        .expect("hopper resolves through the trait object");
    assert!(
        !hopper.is_empty(),
        "a hopper has a non-empty interaction shape"
    );
    let stone = adapter
        .block_interaction(first_id_named("minecraft:stone"))
        .expect("stone resolves through the trait object");
    assert!(
        stone.is_empty(),
        "an ordinary block has no interaction shape, and empty is not None"
    );
}

#[test]
fn shape_seams_agree_with_the_version_tables_for_every_state() {
    // Guards the delegation itself: a swapped pair of accessors in the `impl`
    // would pass every spot check above and fail here.
    let adapter = seam();
    for id in 0..outline_shapes::STATE_COUNT {
        let direct = outline_shapes::outline_boxes(id).expect("table resolves");
        let through = adapter.block_outline(id).expect("seam resolves");
        assert!(
            std::ptr::eq(direct, through),
            "outline seam disagrees with the version table at state {id}"
        );
        let direct = outline_shapes::interaction_boxes(id).expect("table resolves");
        let through = adapter.block_interaction(id).expect("seam resolves");
        assert!(
            std::ptr::eq(direct, through),
            "interaction seam disagrees with the version table at state {id}"
        );
    }
}

#[test]
fn out_of_range_state_ids_are_none_through_the_shape_seams() {
    let adapter = seam();
    assert!(adapter.block_outline(outline_shapes::STATE_COUNT).is_none());
    assert!(adapter.block_outline(u32::MAX).is_none());
    assert!(
        adapter
            .block_interaction(outline_shapes::STATE_COUNT)
            .is_none()
    );
    assert!(adapter.block_interaction(u32::MAX).is_none());
}

// ---------------------------------------------------------------------------
// Item prototypes
// ---------------------------------------------------------------------------

#[test]
fn item_prototype_seam_returns_real_values_not_the_trait_default() {
    let adapter = seam();

    let helmet = adapter
        .item_prototype("minecraft:diamond_helmet")
        .expect("diamond helmet resolves through the trait object");
    assert_eq!(helmet.max_stack_size, 1);
    assert_eq!(helmet.max_damage, Some(363));
    assert_eq!(
        helmet.equip_slot,
        Some(EquipmentSlot::Head),
        "the seam must carry the equip slot — this is what makes armour placeable"
    );
    assert!(helmet.equippable_by_any_entity);

    let bucket = adapter
        .item_prototype("minecraft:water_bucket")
        .expect("water bucket resolves");
    assert_eq!(bucket.max_stack_size, 1, "not 64");
    assert_eq!(bucket.max_damage, None);
    assert_eq!(bucket.equip_slot, None);

    let wolf_armor = adapter
        .item_prototype("minecraft:wolf_armor")
        .expect("wolf armor resolves");
    assert_eq!(
        wolf_armor.equip_slot,
        Some(EquipmentSlot::Body),
        "animal armour is Body, never Chest"
    );
    assert!(
        !wolf_armor.equippable_by_any_entity,
        "wolf armour restricts allowedEntities"
    );

    assert!(
        adapter.item_prototype("minecraft:not_an_item").is_none(),
        "an unknown item must report unknown, not a guessed 64"
    );
}

#[test]
fn item_prototype_seam_agrees_with_the_version_table_for_every_item() {
    let adapter = seam();
    for id in 0..item_prototypes::ITEM_COUNT as i32 {
        let name = lodestone_data::items::item_name(id).expect("named");
        let direct = item_prototypes::prototype_by_id(id).expect("table resolves");
        let through = adapter.item_prototype(name).expect("seam resolves");
        assert_eq!(
            (
                through.max_stack_size,
                through.max_damage,
                through.equip_slot,
                through.equippable_by_any_entity
            ),
            (
                u32::from(direct.max_stack_size),
                direct.max_damage.map(u32::from),
                direct.equip_slot,
                direct.equippable_by_any_entity
            ),
            "item_prototype seam disagrees with the version table for {name} (id {id})"
        );
    }
}

/// The decoder-side half of the same census: a stack decoded off the wire with an
/// **empty component patch** must still carry the prototype's effective values,
/// because that patch is the only thing the wire ever sends for these three.
///
/// This is the check that would have caught the seam existing but nothing seeding
/// it — the shape of island the census exists to close.
#[test]
fn decoded_stacks_carry_the_prototype_effective_fields() {
    use lodestone_model::{ClientEvent, ConnectionState, Directive};
    use lodestone_v26_2::packet_ids::play;
    use lodestone_world::World;

    // set_cursor_item with 1 × minecraft:diamond_helmet (registry id 998 →
    // VarInt 0xE6 0x07) and an empty component patch.
    let payload = vec![0x01, 0xE6, 0x07, 0x00, 0x00];
    let directives = V770Adapter::new()
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::SET_CURSOR_ITEM,
            &payload,
        )
        .expect("handle set_cursor_item");
    let [Directive::Emit(ClientEvent::CursorItemChanged { item: Some(stack) })] =
        directives.as_slice()
    else {
        panic!("expected a CursorItemChanged with a stack, got {directives:?}");
    };
    assert_eq!(stack.item.to_string(), "minecraft:diamond_helmet");
    assert_eq!(
        stack.components.max_stack_size,
        Some(1),
        "an empty patch must not mean an unknown stack cap"
    );
    assert_eq!(stack.components.max_damage, Some(363));
    assert_eq!(
        stack.components.equippable,
        Some(EquipmentSlot::Head),
        "an empty patch must not mean unequippable — this is the live armour bug"
    );
    assert!(!stack.components.has_unmodeled);
}
