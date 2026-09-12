use super::*;

    // ---- the menu family, real-schedule coverage --------------------------
    //
    // `apply_menus` is registered in `SessionPlugin`'s `NetIngest` chain and
    // `lodestone_game::menus::Menus::apply` is exhaustively unit-tested in
    // `lodestone-game`, but until this test nothing in *this* crate ever fed
    // `ScreenOpened`/`ContainerContent`/`ContainerSlot`/`ContainerData`/
    // `CursorItemChanged`/`InventorySlotChanged` through the real schedule —
    // the exact closed-loop shape `CLAUDE.md` names: a fold can be correct and
    // green in its own crate while the wiring one layer up is untested and
    // could be silently broken.

    fn key(s: &str) -> lodestone_model::ids::ResourceKey {
        s.parse().expect("valid resource key")
    }

    fn ms(item: &str, count: u32) -> lodestone_model::ItemStack {
        lodestone_model::ItemStack::new(key(item), count)
    }

    #[test]
    fn menu_family_events_reach_session_menus_through_the_real_schedule() {
        let (mut app, entity) = session_app();

        // Pre-condition: nothing open yet.
        assert_eq!(
            app.world().get::<SessionMenus>(entity).unwrap().0.opened_window_id(),
            None
        );

        fold(
            &mut app,
            ClientEvent::ScreenOpened {
                window_id: 5,
                menu_type: key("minecraft:generic_9x3"),
                title: Text::literal("Chest"),
            },
        );
        // `ScreenOpened` alone only records a *pending* open — `opened` stays
        // `None` until the content packet arrives. Asserting that here is what
        // proves the value actually came from `ScreenOpened`'s own fold rather
        // than being some other default: if this fold never ran, the metadata
        // below (title / menu type) could not appear either.
        assert_eq!(
            app.world().get::<SessionMenus>(entity).unwrap().0.opened_window_id(),
            None,
            "ScreenOpened must not open a menu by itself"
        );

        // A 9x3 chest: 27 container slots + 36 player-inventory slots.
        let mut items = vec![None; 63];
        items[0] = Some(ms("minecraft:gold_ingot", 5));
        fold(
            &mut app,
            ClientEvent::ContainerContent {
                window_id: 5,
                state_id: lodestone_model::ContainerStateId::new(1),
                items,
                carried_item: None,
            },
        );
        {
            let menus = &app.world().get::<SessionMenus>(entity).unwrap().0;
            assert_eq!(menus.opened_window_id(), Some(5));
            assert_eq!(menus.opened_title(), Some(&Text::literal("Chest")));
            assert_eq!(menus.opened_menu_type(), Some(&key("minecraft:generic_9x3")));
            assert!(
                menus.opened().unwrap().slot_item(0).is_some(),
                "the gold ingot from ContainerContent must have landed"
            );
        }

        fold(
            &mut app,
            ClientEvent::ContainerSlot {
                window_id: 5,
                state_id: lodestone_model::ContainerStateId::new(1),
                slot: 1,
                item: Some(ms("minecraft:diamond", 1)),
            },
        );
        assert!(
            app.world()
                .get::<SessionMenus>(entity)
                .unwrap()
                .0
                .opened()
                .unwrap()
                .slot_item(1)
                .is_some(),
            "ContainerSlot must reach the open menu"
        );

        fold(
            &mut app,
            ClientEvent::ContainerData {
                window_id: 5,
                property: 0,
                value: 42,
            },
        );
        assert_eq!(
            app.world()
                .get::<SessionMenus>(entity)
                .unwrap()
                .0
                .container_data(0),
            Some(42),
            "ContainerData must reach the open menu's properties"
        );

        fold(
            &mut app,
            ClientEvent::CursorItemChanged {
                item: Some(ms("minecraft:apple", 1)),
            },
        );
        assert!(
            app.world()
                .get::<SessionMenus>(entity)
                .unwrap()
                .0
                .opened()
                .unwrap()
                .carried()
                .is_some(),
            "CursorItemChanged must reach the carried-item slot"
        );

        fold(
            &mut app,
            ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(ms("minecraft:stone", 32)),
            },
        );
        assert!(
            app.world()
                .get::<SessionMenus>(entity)
                .unwrap()
                .0
                .player_native(0)
                .is_some(),
            "InventorySlotChanged must reach the player-inventory native slots"
        );

        fold(&mut app, ClientEvent::ScreenClosed { window_id: 5 });
        assert_eq!(
            app.world().get::<SessionMenus>(entity).unwrap().0.opened_window_id(),
            None,
            "ScreenClosed must close the menu"
        );
    }
