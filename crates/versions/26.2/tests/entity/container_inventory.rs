//! Hermetic tests for protocol 776 clientbound inventory packets.
//!
//! Golden byte vectors are hand-built from the 26.2 wire spec so a symmetric
//! bug cannot pass silently. An item stack is `count VarInt` (`<= 0` empty),
//! `item id VarInt`, then a data-component patch (`added VarInt`,
//! `removed VarInt`). `container id` and `state id` are VarInts; slot/property/
//! value are big-endian shorts. Every payload is exercised through the public
//! adapter, and every decode asserts zero trailing bytes.
//!
//! Also covers `set_held_slot`, `set_experience`, `set_cursor_item`,
//! `set_player_inventory`, and `cooldown`, which share the same item-stack and
//! VarInt building blocks.

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, ItemStack, ResourceKey, VersionAdapter,
};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;

fn handle(id: i32, payload: &[u8]) -> Vec<Directive> {
    V770Adapter::new()
        .handle_packet(&mut World::new(), ConnectionState::Play, id, payload)
        .expect("handle inventory packet")
}

fn key(s: &str) -> ResourceKey {
    s.parse().expect("valid key")
}

/// `64 × minecraft:stone` (item id 1) with an empty component patch.
const STONE_64: [u8; 4] = [0x40, 0x01, 0x00, 0x00];
/// The empty stack.
const EMPTY_STACK: [u8; 1] = [0x00];

/// The components a decoded `minecraft:stone` stack carries.
///
/// An empty component *patch* does not mean empty components: the decoder folds
/// the item's built-in prototype into [`ItemComponents`]' effective fields, so a
/// plain stone stack reports its real stack cap. The literals below come from the
/// committed server dump (`tests/support/item_prototype_jvm.txt`,
/// `P 1 minecraft:stone 64 - 0 - -`) rather than from the census code, so this is
/// still an external expectation — see `docs/item-prototypes.md`.
fn stone_components() -> lodestone_model::ItemComponents {
    lodestone_model::ItemComponents {
        max_stack_size: Some(64),
        max_damage: None,
        equippable: None,
        ..lodestone_model::ItemComponents::default()
    }
}

#[test]
fn container_set_slot_decodes_a_plain_stack() {
    // window 1, state 5, slot 36, then the stone stack.
    let mut payload = vec![0x01, 0x05, 0x00, 0x24];
    payload.extend_from_slice(&STONE_64);
    match handle(play::clientbound::CONTAINER_SET_SLOT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerSlot {
                window_id,
                state_id,
                slot,
                item,
            }),
        ] => {
            assert_eq!(*window_id, 1);
            assert_eq!(*state_id, 5);
            assert_eq!(*slot, 36);
            assert_eq!(
                *item,
                Some(ItemStack {
                    item: key("minecraft:stone"),
                    count: 64,
                    components: stone_components(),
                })
            );
        }
        other => panic!("expected ContainerSlot, got {other:?}"),
    }
}

#[test]
fn container_set_slot_decodes_the_empty_stack() {
    let mut payload = vec![0x01, 0x01, 0x00, 0x00];
    payload.extend_from_slice(&EMPTY_STACK);
    match handle(play::clientbound::CONTAINER_SET_SLOT, &payload).as_slice() {
        [Directive::Emit(ClientEvent::ContainerSlot { item, .. })] => assert_eq!(*item, None),
        other => panic!("expected ContainerSlot, got {other:?}"),
    }
}

/// `minecraft:dyed_color` (issue: armour dye's feed) — a dyed leather helmet
/// (item id 982, `minecraft:leather_helmet`, per
/// `lodestone_data::generated::items::ITEM_NAMES`) with one added component,
/// `minecraft:dyed_color` (registry id 44, per
/// `lodestone_data::generated::data_component_types::DATA_COMPONENT_TYPE_NAMES`),
/// whose payload is `vanilla's own dyed item color's own stream codec` — a bare
/// `vanilla's own byte buf codecs's own int` (`vanilla's own dyed item color's own java`), i.e. 4 big-endian bytes, not
/// a `VarInt` like every other scalar component this file exercises. The rgb
/// `0x00336699` is arbitrary; the point is that it survives the wire exactly,
/// unmangled by a VarInt reader that would stop after the first `0x80`-set
/// byte (`0xD6, 0x07` alone, i.e. would misread as the *item id*'s
/// continuation, not this field).
#[test]
fn container_set_slot_decodes_a_dyed_leather_helmet() {
    let mut payload = vec![0x01, 0x05, 0x00, 0x24];
    payload.extend_from_slice(&[
        0x01, // count = 1
        0xD6, 0x07, // item id 982 = minecraft:leather_helmet, VarInt
        0x01, // added = 1
        0x00, // removed = 0
        0x2C, // component type id 44 = minecraft:dyed_color
        0x00, 0x33, 0x66, 0x99, // rgb, big-endian i32
    ]);
    match handle(play::clientbound::CONTAINER_SET_SLOT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerSlot { item, .. }),
        ] => {
            let item = item.as_ref().expect("a non-empty stack");
            assert_eq!(item.item, key("minecraft:leather_helmet"));
            assert_eq!(
                item.components.dyed_color,
                Some(0x0033_6699),
                "the dyed_color patch must decode to the exact wire rgb"
            );
        }
        other => panic!("expected ContainerSlot, got {other:?}"),
    }
}

/// `minecraft:trim`. The point of this gate is **not** that the trim
/// decodes — it is that a component after it still does.
///
/// Before the `minecraft:trim` arm existed, this component fell to
/// `read_component_patch`'s `other =>` cliff, which cannot skip an unmodeled
/// payload (clientbound stacks use `vanilla's own data component patch's own stream codec`, undelimited
/// — see that arm's own comment). So a trimmed stack lost the trim *and* every
/// component after it *and* the rest of the packet. The second component and the
/// clean `ensure_empty` are what prove the cliff is gone.
///
/// The trim payload is two `Holder`s in reference form (`registryId + 1`).
///
/// # Why these two ids
///
/// A dynamic registry's holder ids are its JSON entries **sorted by resource
/// id** (`vanilla's own resource manager registry load task's own load`'s
/// `.sorted(vanilla's own entry's own comparing by key())`), which for these all-`minecraft`
/// registries is alphabetical order of the file stems in
/// `data/minecraft/trim_material/` and `data/minecraft/trim_pattern/`. This
/// gate previously carried ids read off the matching `*.bootstrap` *datagen*
/// routine instead, and so asserted the exact mis-mapping the decoder had:
/// its expected values were calibrated against the bug.
///
/// Both ids are therefore chosen to **discriminate** between the two
/// hypotheses rather than merely to be valid:
///
/// | wire | registry id | correct (sorted) | bootstrap order |
/// | --- | --- | --- | --- |
/// | `0x04` | 3 | `emerald` | `redstone` |
/// | `0x09` | 8 | `sentry` | `snout` |
///
/// The material row is the pair the bug was reported as — an emerald trim
/// rendering as redstone.
#[test]
fn container_set_slot_decodes_a_trimmed_chestplate_without_losing_the_rest_of_the_patch() {
    let mut payload = vec![0x01, 0x06, 0x00, 0x25];
    payload.extend_from_slice(&[
        0x01, // count = 1
        0xD9, 0x07, // item id 985, VarInt
        0x02, // added = 2
        0x00, // removed = 0
        0x38, // component type id 56 = minecraft:trim
        0x04, // Holder<TrimMaterial>: reference, registry id 3 = emerald
        0x09, // Holder<TrimPattern>: reference, registry id 8 = sentry
        0x03, // component type id 3 = minecraft:damage
        0x07, // damage = 7, VarInt
    ]);
    match handle(play::clientbound::CONTAINER_SET_SLOT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerSlot { item, .. }),
        ] => {
            let item = item.as_ref().expect("a non-empty stack");
            let trim = item.components.trim.as_ref().expect("the trim component");
            assert_eq!(
                trim.material, "emerald",
                "registry id 3 is the fourth trim_material sorted by resource id; \
                 \"redstone\" here means the table is back in bootstrap order"
            );
            assert_eq!(
                trim.pattern, "sentry",
                "registry id 8 is the ninth trim_pattern sorted by resource id; \
                 \"snout\" here means the table is back in bootstrap order"
            );
            // The cliff: this is the component *after* the trim.
            assert_eq!(
                item.components.damage,
                Some(7),
                "a component after the trim must still decode — the whole point of \
                 modeling trim rather than letting it stop the patch"
            );
            assert!(
                !item.components.has_unmodeled,
                "nothing in this patch is unmodeled, so the partial-stack flag must stay clear"
            );
        }
        other => panic!("expected ContainerSlot, got {other:?}"),
    }
}

#[test]
fn container_set_content_decodes_items_and_carried() {
    // window 1, state 2, two items [stone, empty], carried empty.
    let mut payload = vec![0x01, 0x02, 0x02];
    payload.extend_from_slice(&STONE_64);
    payload.extend_from_slice(&EMPTY_STACK); // second slot empty
    payload.extend_from_slice(&EMPTY_STACK); // carried empty
    match handle(play::clientbound::CONTAINER_SET_CONTENT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerContent {
                window_id,
                state_id,
                items,
                carried_item,
            }),
        ] => {
            assert_eq!(*window_id, 1);
            assert_eq!(*state_id, 2);
            assert_eq!(items.len(), 2);
            assert_eq!(items[0].as_ref().map(|s| s.count), Some(64));
            assert_eq!(items[1], None);
            assert_eq!(*carried_item, None);
        }
        other => panic!("expected ContainerContent, got {other:?}"),
    }
}

#[test]
fn container_set_data_decodes_property_channel() {
    // window 1, property 0, value 200 (0x00C8).
    let payload = [0x01, 0x00, 0x00, 0x00, 0xC8];
    match handle(play::clientbound::CONTAINER_SET_DATA, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerData {
                window_id,
                property,
                value,
            }),
        ] => {
            assert_eq!(*window_id, 1);
            assert_eq!(*property, 0);
            assert_eq!(*value, 200);
        }
        other => panic!("expected ContainerData, got {other:?}"),
    }
}

#[test]
fn container_close_decodes_window_id() {
    let payload = [0x07];
    match handle(play::clientbound::CONTAINER_CLOSE, &payload).as_slice() {
        [Directive::Emit(ClientEvent::ScreenClosed { window_id })] => assert_eq!(*window_id, 7),
        other => panic!("expected ScreenClosed, got {other:?}"),
    }
}

#[test]
fn item_stack_with_component_patch_is_refused_loudly() {
    // window 1, state 1, slot 1, then item: count 1, id 1, added=1, removed=0 —
    // a non-empty patch. The decoder must refuse rather than misparse the
    // un-length-prefixed component bytes.
    let payload = vec![0x01, 0x01, 0x00, 0x01, 0x01, 0x01, 0x01, 0x00];
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::CONTAINER_SET_SLOT,
        &payload,
    );
    assert!(
        result.is_err(),
        "a non-empty component patch must be refused, got {result:?}"
    );
}

#[test]
fn container_set_slot_rejects_trailing_bytes() {
    let mut payload = vec![0x01, 0x05, 0x00, 0x24];
    payload.extend_from_slice(&STONE_64);
    payload.push(0xFF); // one stray byte
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::CONTAINER_SET_SLOT,
        &payload,
    );
    assert!(
        result.is_err(),
        "a trailing byte must fail decode, got {result:?}"
    );
}

// ---- set_held_slot ---------------------------------------------------------

#[test]
fn set_held_slot_emits_slot() {
    let directives = handle(play::clientbound::SET_HELD_SLOT, &[0x04]);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::HeldSlotChanged { slot: 4 })]
    );
}

#[test]
fn set_held_slot_rejects_trailing_bytes() {
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_HELD_SLOT,
        &[0x04, 0xFF],
    );
    assert!(
        result.is_err(),
        "a misaligned set_held_slot must be rejected"
    );
}

// ---- set_experience ---------------------------------------------------------

#[test]
fn set_experience_decodes_progress_level_total_wire_order() {
    // Wire order is progress (f32), level (varint), total (varint) — not the
    // constructor's declared field order.
    let mut payload = 0.5f32.to_be_bytes().to_vec();
    payload.push(0x1E); // level 30
    payload.push(0x64); // total 100
    let directives = handle(play::clientbound::SET_EXPERIENCE, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ExperienceChanged {
            progress: 0.5,
            level: 30,
            total: 100,
        })]
    );
}

#[test]
fn set_experience_rejects_trailing_bytes() {
    let mut payload = 0.0f32.to_be_bytes().to_vec();
    payload.push(0x00);
    payload.push(0x00);
    payload.push(0xFF);
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_EXPERIENCE,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned set_experience must be rejected"
    );
}

// ---- set_cursor_item ---------------------------------------------------------

#[test]
fn set_cursor_item_decodes_a_plain_stack() {
    let directives = handle(play::clientbound::SET_CURSOR_ITEM, &STONE_64);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::CursorItemChanged {
            item: Some(ItemStack {
                item: key("minecraft:stone"),
                count: 64,
                components: stone_components(),
            }),
        })]
    );
}

#[test]
fn set_cursor_item_decodes_the_empty_stack() {
    let directives = handle(play::clientbound::SET_CURSOR_ITEM, &EMPTY_STACK);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::CursorItemChanged {
            item: None
        })]
    );
}

#[test]
fn set_cursor_item_rejects_trailing_bytes() {
    let mut payload = STONE_64.to_vec();
    payload.push(0xFF);
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_CURSOR_ITEM,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned set_cursor_item must be rejected"
    );
}

// ---- set_player_inventory ---------------------------------------------------

#[test]
fn set_player_inventory_decodes_slot_and_stack() {
    let mut payload = vec![0x08]; // slot 8
    payload.extend_from_slice(&STONE_64);
    let directives = handle(play::clientbound::SET_PLAYER_INVENTORY, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::InventorySlotChanged {
            slot: 8,
            item: Some(ItemStack {
                item: key("minecraft:stone"),
                count: 64,
                components: stone_components(),
            }),
        })]
    );
}

#[test]
fn set_player_inventory_rejects_trailing_bytes() {
    let mut payload = vec![0x08];
    payload.extend_from_slice(&STONE_64);
    payload.push(0xFF);
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::SET_PLAYER_INVENTORY,
        &payload,
    );
    assert!(
        result.is_err(),
        "a misaligned set_player_inventory must be rejected"
    );
}

// ---- cooldown ---------------------------------------------------------------

#[test]
fn cooldown_decodes_combined_namespace_path_string() {
    let group = "minecraft:ender_pearl";
    let mut payload = vec![group.len() as u8];
    payload.extend_from_slice(group.as_bytes());
    payload.push(0xA0); // duration_ticks varint low byte (continuation)
    payload.push(0x01); // duration_ticks varint high byte -> 160
    let directives = handle(play::clientbound::COOLDOWN, &payload);
    assert_eq!(
        directives,
        vec![Directive::Emit(ClientEvent::ItemCooldown {
            group: key("minecraft:ender_pearl"),
            duration_ticks: 160,
        })]
    );
}

#[test]
fn cooldown_rejects_trailing_bytes() {
    let group = "minecraft:ender_pearl";
    let mut payload = vec![group.len() as u8];
    payload.extend_from_slice(group.as_bytes());
    payload.push(0x00);
    payload.push(0xFF);
    let result = V770Adapter::new().handle_packet(
        &mut World::new(),
        ConnectionState::Play,
        play::clientbound::COOLDOWN,
        &payload,
    );
    assert!(result.is_err(), "a misaligned cooldown must be rejected");
}
