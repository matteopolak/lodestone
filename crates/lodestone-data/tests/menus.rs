//! Hermetic controls for the committed 26.2 menu registry table.
//!
//! The typed lookup is deliberately tested with literal wire ids as well as
//! the module's generated-table round trip: those controls catch a shifted
//! table without deriving both expected values from the same source.

use lodestone_data::menus::{self, MenuId, MENU_COUNT};

#[test]
fn menu_id_validates_the_table_domain() {
    for raw in 0..MENU_COUNT as i32 {
        let id = MenuId::new(raw).expect("table id validates");
        assert!(
            !menus::menu_name(id).is_empty(),
            "id {id:?} in 0..{MENU_COUNT} did not resolve to a name"
        );
    }

    assert_eq!(MenuId::new(-1), None);
    assert_eq!(MenuId::new(MENU_COUNT as i32), None);
    assert_eq!(MenuId::new(i32::MAX), None);

    let lookup: fn(MenuId) -> &'static str = menus::menu_name;
    let reverse: fn(&str) -> Option<MenuId> = menus::menu_id;
    let furnace = MenuId::new(14).expect("known id validates");
    assert_eq!(reverse(lookup(furnace)), Some(furnace));
}

#[test]
fn literal_wire_ids_resolve_to_their_identifiers() {
    assert_eq!(
        menus::menu_name(MenuId::new(14).expect("furnace id validates")),
        "minecraft:furnace"
    );
    assert_eq!(
        menus::menu_name(MenuId::new(19).expect("merchant id validates")),
        "minecraft:merchant"
    );
}
