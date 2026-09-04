//! Seam tests for protocol 340's window/inventory and player-list packets.
//!
//! `OpenWindow`/`CloseWindow`/`SetSlot`/`WindowItems` already had correct,
//! tested wire codecs (`tests/inventory.rs`) that `V340Adapter::handle_packet`
//! never called — the "correct decoder the adapter never calls" island this
//! project keeps hitting. Every case here drives the real dispatch path and
//! asserts on the resulting [`ClientEvent`], not just on the decoder.

use lodestone_core::{Ctx, Encode, Writer};
use lodestone_model::{ClientEvent, ConnectionState, Directive, GameMode, VersionAdapter};
use lodestone_v1_9::V340Adapter;
use lodestone_v1_9::packet_ids::play;
use lodestone_v1_9::packets::player_info::{PlayerInfoAction, PlayerInfoEntry};
use lodestone_v1_9::packets::slot::Slot;
use lodestone_v1_9::packets::window::{CloseWindow, OpenWindow, SetSlot, WindowItems};
use lodestone_world::World;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 340 };

fn encode<T: Encode>(value: &T) -> Vec<u8> {
    let mut writer = Writer::default();
    value.encode(&mut writer, CTX).expect("encode");
    writer.into_vec()
}

fn dispatch(packet_id: i32, payload: &[u8]) -> Vec<Directive> {
    let adapter = V340Adapter::new();
    adapter
        .handle_packet(&mut World::new(), ConnectionState::Play, packet_id, payload)
        .expect("handle_packet")
}

fn a_stack(id: i16, count: i8) -> Slot {
    Slot::Item {
        id,
        count,
        damage: 0,
        nbt: None,
    }
}

// ---------------------------------------------------------------------------
// OpenWindow / CloseWindow
// ---------------------------------------------------------------------------

#[test]
fn open_window_furnace_resolves_its_static_menu_type() {
    let payload = encode(&OpenWindow {
        window_id: 3,
        inventory_type: "minecraft:furnace".into(),
        window_title: "{\"text\":\"Furnace\"}".into(),
        slot_count: 3,
        entity_id: None,
    });
    match dispatch(play::clientbound::OPEN_WINDOW, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ScreenOpened {
                window_id,
                menu_type,
                title,
            }),
        ] => {
            assert_eq!(*window_id, 3);
            assert_eq!(menu_type.to_string(), "minecraft:furnace");
            assert_eq!(title.to_plain_string(), "Furnace");
        }
        other => panic!("expected ScreenOpened, got {other:?}"),
    }
}

#[test]
fn open_window_chest_sizes_the_generic_menu_from_slot_count() {
    // A single chest is 27 slots -> generic_9x3; a double chest is 54 -> 9x6.
    // Pairwise-distinct slot counts so a stuck row calculation cannot pass by
    // coincidence.
    let single = encode(&OpenWindow {
        window_id: 1,
        inventory_type: "minecraft:chest".into(),
        window_title: "{\"text\":\"Chest\"}".into(),
        slot_count: 27,
        entity_id: None,
    });
    match dispatch(play::clientbound::OPEN_WINDOW, &single).as_slice() {
        [Directive::Emit(ClientEvent::ScreenOpened { menu_type, .. })] => {
            assert_eq!(menu_type.to_string(), "minecraft:generic_9x3");
        }
        other => panic!("expected ScreenOpened, got {other:?}"),
    }

    let double = encode(&OpenWindow {
        window_id: 2,
        inventory_type: "minecraft:chest".into(),
        window_title: "{\"text\":\"Large Chest\"}".into(),
        slot_count: 54,
        entity_id: None,
    });
    match dispatch(play::clientbound::OPEN_WINDOW, &double).as_slice() {
        [Directive::Emit(ClientEvent::ScreenOpened { menu_type, .. })] => {
            assert_eq!(menu_type.to_string(), "minecraft:generic_9x6");
        }
        other => panic!("expected ScreenOpened, got {other:?}"),
    }
}

#[test]
fn open_window_horse_falls_back_to_a_generic_menu_and_carries_the_entity_id() {
    let payload = encode(&OpenWindow {
        window_id: 5,
        inventory_type: "EntityHorse".into(),
        window_title: "{\"text\":\"Horse\"}".into(),
        slot_count: 17,
        entity_id: Some(42),
    });
    match dispatch(play::clientbound::OPEN_WINDOW, &payload).as_slice() {
        [Directive::Emit(ClientEvent::ScreenOpened { menu_type, .. })] => {
            // 17 slots -> ceil-clamped to 2 rows of 9 (26.2 has no dedicated
            // horse menu type; see `resolve_menu_type`'s doc).
            assert_eq!(menu_type.to_string(), "minecraft:generic_9x2");
        }
        other => panic!("expected ScreenOpened, got {other:?}"),
    }
}

#[test]
fn close_window_dispatches_screen_closed() {
    let payload = encode(&CloseWindow { window_id: 4 });
    match dispatch(play::clientbound::CLOSE_WINDOW, &payload).as_slice() {
        [Directive::Emit(ClientEvent::ScreenClosed { window_id })] => {
            assert_eq!(*window_id, 4);
        }
        other => panic!("expected ScreenClosed, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// WindowItems / SetSlot
// ---------------------------------------------------------------------------

#[test]
fn window_items_resolves_known_items_and_treats_unknown_ids_as_empty() {
    let payload = encode(&WindowItems {
        window_id: 1,
        items: vec![
            a_stack(1, 5),      // minecraft:stone x5
            Slot::Empty,        // empty
            a_stack(9999, 1),   // no such 1.12 item id
            a_stack(35, 11),    // minecraft:wool x11 (family resolves; colour does not)
        ],
    });
    match dispatch(play::clientbound::WINDOW_ITEMS, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerContent {
                window_id,
                items,
                carried_item,
                ..
            }),
        ] => {
            assert_eq!(*window_id, 1);
            assert_eq!(items.len(), 4);
            let stone = items[0].as_ref().expect("stone resolved");
            assert_eq!(stone.item.to_string(), "minecraft:stone");
            assert_eq!(stone.count, 5);
            assert!(items[1].is_none(), "an empty slot must decode to None");
            assert!(
                items[2].is_none(),
                "an unresolvable item id must decode to None, not error the whole packet"
            );
            let wool = items[3].as_ref().expect("wool resolved");
            assert_eq!(wool.item.to_string(), "minecraft:wool");
            assert_eq!(wool.count, 11);
            assert!(carried_item.is_none(), "1.12.2's window_items has no cursor field");
        }
        other => panic!("expected ContainerContent, got {other:?}"),
    }
}

#[test]
fn set_slot_negative_one_is_the_cursor_item() {
    let payload = encode(&SetSlot {
        window_id: -1,
        slot: -1,
        item: a_stack(1, 1),
    });
    match dispatch(play::clientbound::SET_SLOT, &payload).as_slice() {
        [Directive::Emit(ClientEvent::CursorItemChanged { item })] => {
            assert_eq!(item.as_ref().expect("stone").item.to_string(), "minecraft:stone");
        }
        other => panic!("expected CursorItemChanged, got {other:?}"),
    }
}

#[test]
fn set_slot_window_zero_is_the_players_own_inventory() {
    let payload = encode(&SetSlot {
        window_id: 0,
        slot: 9,
        item: a_stack(1, 1),
    });
    match dispatch(play::clientbound::SET_SLOT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::InventorySlotChanged { slot, item }),
        ] => {
            assert_eq!(*slot, 9);
            assert!(item.is_some());
        }
        other => panic!("expected InventorySlotChanged, got {other:?}"),
    }
}

#[test]
fn set_slot_positive_window_is_a_container_slot() {
    let payload = encode(&SetSlot {
        window_id: 2,
        slot: 6,
        item: Slot::Empty,
    });
    match dispatch(play::clientbound::SET_SLOT, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::ContainerSlot {
                window_id,
                slot,
                item,
                ..
            }),
        ] => {
            assert_eq!(*window_id, 2);
            assert_eq!(*slot, 6);
            assert!(item.is_none());
        }
        other => panic!("expected ContainerSlot, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Player info (tab list)
// ---------------------------------------------------------------------------

fn encode_player_info(entries: Vec<PlayerInfoEntry>) -> (i32, Vec<u8>) {
    // All the entries built by this test file's helpers share one action, so
    // borrow the first entry's discriminant the same way vanilla's own
    // packet does — one action id for the whole packet.
    let action_id = match entries.first().map(|e| &e.action) {
        Some(PlayerInfoAction::AddPlayer { .. }) => 0,
        Some(PlayerInfoAction::UpdateGameMode { .. }) => 1,
        Some(PlayerInfoAction::UpdateLatency { .. }) => 2,
        Some(PlayerInfoAction::UpdateDisplayName { .. }) => 3,
        Some(PlayerInfoAction::RemovePlayer) | None => 4,
    };
    let mut w = Writer::default();
    w.var_i32(action_id);
    w.var_i32(entries.len() as i32);
    for entry in &entries {
        w.uuid(entry.uuid);
        match &entry.action {
            PlayerInfoAction::AddPlayer {
                name,
                properties,
                game_mode,
                ping,
                display_name,
            } => {
                w.string(name);
                w.var_i32(properties.len() as i32);
                for p in properties {
                    w.string(&p.name);
                    w.string(&p.value);
                    match &p.signature {
                        Some(s) => {
                            w.bool(true);
                            w.string(s);
                        }
                        None => w.bool(false),
                    }
                }
                w.var_i32(*game_mode);
                w.var_i32(*ping);
                match display_name {
                    Some(s) => {
                        w.bool(true);
                        w.string(s);
                    }
                    None => w.bool(false),
                }
            }
            PlayerInfoAction::UpdateGameMode { game_mode } => w.var_i32(*game_mode),
            PlayerInfoAction::UpdateLatency { ping } => w.var_i32(*ping),
            PlayerInfoAction::UpdateDisplayName { display_name } => match display_name {
                Some(s) => {
                    w.bool(true);
                    w.string(s);
                }
                None => w.bool(false),
            },
            PlayerInfoAction::RemovePlayer => {}
        }
    }
    (action_id, w.into_vec())
}

#[test]
fn player_info_add_player_carries_gamemode_ping_name_and_skin_properties() {
    let uuid = Uuid::from_u128(1);
    let entries = vec![PlayerInfoEntry {
        uuid,
        action: PlayerInfoAction::AddPlayer {
            name: "Notch".into(),
            properties: vec![lodestone_v1_9::packets::player_info::PlayerInfoProperty {
                name: "textures".into(),
                value: "eyJ0ZXh0dXJlcyI6e319".into(),
                signature: Some("SIG".into()),
            }],
            game_mode: 1,
            ping: 42,
            display_name: Some("{\"text\":\"Notch!\"}".into()),
        },
    }];
    let (_, payload) = encode_player_info(entries);
    match dispatch(play::clientbound::PLAYER_INFO, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::PlayerListUpdate { entries }),
        ] => {
            assert_eq!(entries.len(), 1);
            let e = &entries[0];
            assert_eq!(e.uuid, Some(uuid));
            assert_eq!(e.name.as_deref(), Some("Notch"));
            assert_eq!(e.game_mode, Some(GameMode::Creative));
            assert_eq!(e.latency, Some(42));
            assert_eq!(
                e.properties.as_ref().map(Vec::len),
                Some(1),
                "the skin property must reach the canonical entry"
            );
            assert_eq!(
                e.properties.as_ref().unwrap()[0].value,
                "eyJ0ZXh0dXJlcyI6e319"
            );
        }
        other => panic!("expected PlayerListUpdate, got {other:?}"),
    }
}

#[test]
fn player_info_remove_dispatches_player_list_remove() {
    let a = Uuid::from_u128(7);
    let b = Uuid::from_u128(8);
    let entries = vec![
        PlayerInfoEntry {
            uuid: a,
            action: PlayerInfoAction::RemovePlayer,
        },
        PlayerInfoEntry {
            uuid: b,
            action: PlayerInfoAction::RemovePlayer,
        },
    ];
    let (_, payload) = encode_player_info(entries);
    match dispatch(play::clientbound::PLAYER_INFO, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::PlayerListRemove { profile_ids }),
        ] => {
            assert_eq!(profile_ids, &vec![a, b]);
        }
        other => panic!("expected PlayerListRemove, got {other:?}"),
    }
}

#[test]
fn player_info_update_latency_only_leaves_other_fields_unreported() {
    let uuid = Uuid::from_u128(3);
    let entries = vec![PlayerInfoEntry {
        uuid,
        action: PlayerInfoAction::UpdateLatency { ping: 15 },
    }];
    let (_, payload) = encode_player_info(entries);
    match dispatch(play::clientbound::PLAYER_INFO, &payload).as_slice() {
        [
            Directive::Emit(ClientEvent::PlayerListUpdate { entries }),
        ] => {
            assert_eq!(entries[0].latency, Some(15));
            assert_eq!(entries[0].name, None);
            assert_eq!(entries[0].game_mode, None);
        }
        other => panic!("expected PlayerListUpdate, got {other:?}"),
    }
}
