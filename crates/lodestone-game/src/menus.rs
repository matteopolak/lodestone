//! The open-menu session: the player's own inventory plus at most one open
//! container, folded from the server's container [`ClientEvent`]s so the UI can
//! draw them.
//!
//! This is the consumer the container packets were missing. `open_screen`,
//! `container_set_content`, `container_set_slot`, `set_cursor_item`,
//! `set_player_inventory`, `container_set_data` and `container_close` all decode
//! and emit `ClientEvent`s, but until something folds them they are
//! decode-and-discard with an extra hop. [`Menus::apply`] routes each into the
//! [`ClientMenu`] predict/reconcile seam (for the server-authoritative slot and
//! content updates) or the menu-lifecycle bookkeeping here (for open/close).
//!
//! Two slot-numbering hazards are handled explicitly, because getting either
//! wrong renders a plausible but transposed inventory that looks like an art
//! bug:
//!
//! * `container_set_content` / `container_set_slot` address slots in **menu
//!   order** — window 0 is `0` result, `1..=4` craft, `5..=8` armour, `9..=35`
//!   main, `36..=44` hotbar, `45` off-hand; a generic container is `0..n`
//!   container, `n..n+27` main, `n+27..n+36` hotbar with no armour/off-hand.
//!   These map straight onto [`Menu`] indices.
//! * `set_player_inventory` ([`ClientEvent::InventorySlotChanged`]) addresses
//!   the player inventory in **native order** (`0..=8` hotbar, `9..=35` main,
//!   `36..=39` armour, `40` off-hand), a different numbering, so it is applied
//!   via the native path, not the menu-indexed one.
//!
//! The opened container's size is taken from the server's own
//! `container_set_content` (`items.len() - PLAYER_INVENTORY_PORTION`) rather than
//! a hand-written menu-type→size table, so the size originates outside our code.

use lodestone_model::{ClientEvent, ItemStack as ModelItemStack, Text, ids::ResourceKey};

use crate::{
    item::ItemStack,
    menu::Menu,
    recipe::{CraftingGrid, RecipeBook},
    reconcile::{ClientMenu, ServerUpdate},
};

/// The player-inventory portion (main + hotbar) appended to every non-player
/// `container_set_content`. Armour and off-hand are *not* included for open
/// containers, so a generic container's content length is `container_size + 36`.
const PLAYER_INVENTORY_PORTION: usize = 36;

/// A currently open container screen and its server-synchronised menu.
#[derive(Debug, Clone)]
pub struct OpenMenu {
    window_id: i32,
    menu_type: Option<ResourceKey>,
    title: Option<Text>,
    menu: ClientMenu,
    /// Menu-local properties (`container_set_data`), e.g. furnace burn/cook
    /// progress, as `(property_id, value)`.
    data: Vec<(i32, i32)>,
}

impl OpenMenu {
    fn set_data(&mut self, property: i32, value: i32) {
        match self.data.iter_mut().find(|(p, _)| *p == property) {
            Some(entry) => entry.1 = value,
            None => self.data.push((property, value)),
        }
    }
}

/// A pending `open_screen` whose contents (and therefore size) have not arrived
/// yet. The menu is not built until the first `container_set_content`.
#[derive(Debug, Clone)]
struct PendingOpen {
    window_id: i32,
    menu_type: ResourceKey,
    title: Text,
}

/// The player's inventory plus at most one open container.
#[derive(Debug, Clone)]
pub struct Menus {
    player: ClientMenu,
    opened: Option<OpenMenu>,
    pending: Option<PendingOpen>,
}

impl Default for Menus {
    fn default() -> Self {
        Self::new()
    }
}

impl Menus {
    /// A fresh session with the player inventory open and no container.
    #[must_use]
    pub fn new() -> Self {
        Self {
            player: ClientMenu::new(Menu::player()),
            opened: None,
            pending: None,
        }
    }

    /// The player's own inventory menu (window 0), always present.
    #[must_use]
    pub fn player(&self) -> &Menu {
        self.player.menu()
    }

    /// The open container menu, if a container screen is open.
    #[must_use]
    pub fn opened(&self) -> Option<&Menu> {
        self.opened.as_ref().map(|o| o.menu.menu())
    }

    /// The menu the UI should draw: the open container if any, else the player
    /// inventory.
    #[must_use]
    pub fn active(&self) -> &Menu {
        match &self.opened {
            Some(o) => o.menu.menu(),
            None => self.player.menu(),
        }
    }

    /// The window id of the open container, if any.
    #[must_use]
    pub fn opened_window_id(&self) -> Option<i32> {
        self.opened.as_ref().map(|o| o.window_id)
    }

    /// The open container's title, if known.
    #[must_use]
    pub fn opened_title(&self) -> Option<&Text> {
        self.opened.as_ref().and_then(|o| o.title.as_ref())
    }

    /// The open container's canonical menu-type key, if known.
    #[must_use]
    pub fn opened_menu_type(&self) -> Option<&ResourceKey> {
        self.opened.as_ref().and_then(|o| o.menu_type.as_ref())
    }

    /// A menu-local container property (`container_set_data`) of the open
    /// container, e.g. furnace cook progress.
    #[must_use]
    pub fn container_data(&self, property: i32) -> Option<i32> {
        self.opened
            .as_ref()?
            .data
            .iter()
            .find(|(p, _)| *p == property)
            .map(|(_, v)| *v)
    }

    /// Folds a container [`ClientEvent`] into the session, returning `true` if
    /// the event was one this state owns. Same fan-out contract as the other
    /// game aggregates' `apply` methods.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::ScreenOpened {
                window_id,
                menu_type,
                title,
            } => {
                // Size is unknown until the content packet; record the metadata
                // and build the menu when container_set_content arrives.
                self.pending = Some(PendingOpen {
                    window_id: *window_id,
                    menu_type: menu_type.clone(),
                    title: title.clone(),
                });
            }
            ClientEvent::ScreenClosed { window_id } => {
                if self
                    .opened
                    .as_ref()
                    .is_some_and(|o| o.window_id == *window_id)
                {
                    self.opened = None;
                }
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|p| p.window_id == *window_id)
                {
                    self.pending = None;
                }
            }
            ClientEvent::ContainerContent {
                window_id,
                state_id,
                items,
                carried_item,
            } => {
                let update = ServerUpdate::SetContent {
                    state_id: *state_id as u32,
                    items: items.iter().map(model_item_to_game).collect(),
                    carried: model_item_to_game(carried_item),
                };
                if *window_id == 0 {
                    self.player.reconcile(update);
                } else {
                    self.ensure_open(*window_id, items.len()).reconcile(update);
                }
            }
            ClientEvent::ContainerSlot {
                window_id,
                state_id,
                slot,
                item,
            } => {
                if let Ok(slot) = usize::try_from(*slot)
                    && let Some(menu) = self.menu_for_mut(*window_id)
                {
                    menu.reconcile(ServerUpdate::SetSlot {
                        state_id: *state_id as u32,
                        slot,
                        item: model_item_to_game(item),
                    });
                }
            }
            ClientEvent::ContainerData {
                window_id,
                property,
                value,
            } => {
                if let Some(open) = self.opened.as_mut().filter(|o| o.window_id == *window_id) {
                    open.set_data(*property, *value);
                }
            }
            ClientEvent::CursorItemChanged { item } => {
                // The cursor is shared; drive whichever menu the UI is showing.
                let menu = match &mut self.opened {
                    Some(open) => &mut open.menu,
                    None => &mut self.player,
                };
                menu.reconcile(ServerUpdate::SetCarried {
                    item: model_item_to_game(item),
                });
            }
            ClientEvent::InventorySlotChanged { slot, item } => {
                if let Ok(native) = usize::try_from(*slot) {
                    self.player
                        .set_player_native(native, model_item_to_game(item));
                }
            }
            _ => return false,
        }
        true
    }

    /// Returns the [`ClientMenu`] for a window id, or `None` if the id is neither
    /// the player inventory nor the open container.
    fn menu_for_mut(&mut self, window_id: i32) -> Option<&mut ClientMenu> {
        if window_id == 0 {
            return Some(&mut self.player);
        }
        self.opened
            .as_mut()
            .filter(|o| o.window_id == window_id)
            .map(|o| &mut o.menu)
    }

    /// Ensures an open container menu exists for `window_id`, (re)building it
    /// sized from the server's content length, and returns it.
    fn ensure_open(&mut self, window_id: i32, content_len: usize) -> &mut ClientMenu {
        let matches = self
            .opened
            .as_ref()
            .is_some_and(|o| o.window_id == window_id);
        if !matches {
            let container_size = content_len.saturating_sub(PLAYER_INVENTORY_PORTION);
            let (menu_type, title) = match self.pending.take() {
                Some(p) if p.window_id == window_id => (Some(p.menu_type), Some(p.title)),
                other => {
                    // A content packet for a window we never saw opened: keep the
                    // menu but leave its metadata unknown, and preserve any
                    // pending open for its own window.
                    self.pending = other;
                    (None, None)
                }
            };
            let menu = build_menu(menu_type.as_ref(), container_size);
            self.opened = Some(OpenMenu {
                window_id,
                menu_type,
                title,
                menu: ClientMenu::new(menu),
                data: Vec::new(),
            });
        }
        &mut self.opened.as_mut().expect("just set").menu
    }

    /// The active menu's crafting grid contents, if it has a crafting grid.
    ///
    /// This is the player's 2×2 on the inventory screen and the 3×3 in an open
    /// crafting table.
    #[must_use]
    pub fn crafting_grid(&self) -> Option<CraftingGrid> {
        self.active().crafting_grid()
    }

    /// The result `book` says the active menu's crafting grid would produce.
    ///
    /// **This is a prediction, not the truth.** A vanilla server computes the
    /// result itself and pushes it as a `container_set_slot` for the result
    /// slot, which [`apply`](Self::apply) already reconciles into the menu; read
    /// the result slot for what the player is actually holding a claim to. Use
    /// this for a ghost/preview, for showing a result before the round-trip
    /// lands, or when there is no server at all.
    #[must_use]
    pub fn predicted_craft_result(&self, book: &RecipeBook) -> Option<ItemStack> {
        let grid = self.crafting_grid()?;
        if grid.is_empty() {
            return None;
        }
        book.match_grid(&grid).cloned()
    }
}

/// Chooses the [`Menu`] layout for an opened window.
///
/// The **size** always comes from the server's own content length; `menu_type`
/// only selects the slot *kinds*. A crafting table advertises
/// `minecraft:crafting`, whose `CraftingMenu` is `1 + 3*3 = 10` container slots;
/// if the server disagrees about the size we fall back to a generic container
/// rather than build a menu whose slot count contradicts the packet.
fn build_menu(menu_type: Option<&ResourceKey>, container_size: usize) -> Menu {
    let is_crafting =
        menu_type.is_some_and(|key| key.namespace() == "minecraft" && key.path() == "crafting");
    if is_crafting && container_size == 10 {
        Menu::crafting(3, 3)
    } else {
        Menu::generic(container_size)
    }
}

fn model_item_to_game(item: &Option<ModelItemStack>) -> Option<ItemStack> {
    item.as_ref().map(ItemStack::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::item::ItemStack as GameItemStack;
    use lodestone_model::ItemStack as ModelItemStack;
    use lodestone_model::ids::Identifier;

    fn id(s: &str) -> Identifier {
        s.parse().expect("valid id")
    }

    fn stack(name: &str, count: u32) -> ModelItemStack {
        ModelItemStack {
            item: id(name),
            count,
            components: lodestone_model::ItemComponents::default(),
        }
    }

    fn game_stack(name: &str, count: i32) -> GameItemStack {
        GameItemStack::new(id(name), count)
    }

    fn key(s: &str) -> ResourceKey {
        s.parse().expect("valid key")
    }

    /// A window-0 (player inventory) content vector of the full 46 menu slots
    /// with the given (menu_index, stack) overrides.
    fn player_items(overrides: &[(usize, ModelItemStack)]) -> Vec<Option<ModelItemStack>> {
        let mut items = vec![None; 46];
        for (i, s) in overrides {
            items[*i] = Some(s.clone());
        }
        items
    }

    #[test]
    fn player_content_folds_in_menu_order() {
        let mut menus = Menus::new();
        // Distinct items at distinct menu positions catch a reorder.
        let items = player_items(&[
            (0, stack("minecraft:diamond", 1)), // result slot
            (36, stack("minecraft:stone", 64)), // first hotbar slot
            (45, stack("minecraft:shield", 1)), // off-hand
        ]);
        assert!(menus.apply(&ClientEvent::ContainerContent {
            window_id: 0,
            state_id: 1,
            items,
            carried_item: None,
        }));
        assert_eq!(
            menus.player().slot_item(0),
            Some(&game_stack("minecraft:diamond", 1))
        );
        assert_eq!(
            menus.player().slot_item(36),
            Some(&game_stack("minecraft:stone", 64))
        );
        assert_eq!(
            menus.player().slot_item(45),
            Some(&game_stack("minecraft:shield", 1))
        );
    }

    #[test]
    fn inventory_slot_changed_uses_native_order_not_menu_order() {
        // The transposition guard: set_player_inventory addresses native slot 0
        // (hotbar[0]), which is menu index 36, NOT menu index 0 (the result
        // slot). A naive menu-indexed apply would land it at 0.
        let mut menus = Menus::new();
        assert!(menus.apply(&ClientEvent::InventorySlotChanged {
            slot: 0,
            item: Some(stack("minecraft:stone", 32)),
        }));
        assert_eq!(
            menus.player().slot_item(36),
            Some(&game_stack("minecraft:stone", 32))
        );
        assert_eq!(menus.player().slot_item(0), None);
    }

    #[test]
    fn open_screen_then_content_builds_generic_sized_from_server() {
        let mut menus = Menus::new();
        assert!(menus.apply(&ClientEvent::ScreenOpened {
            window_id: 5,
            menu_type: key("minecraft:generic_9x3"),
            title: Text::literal("Chest"),
        }));
        // A 9x3 chest: 27 container slots + 36 player = 63 content slots.
        let mut items = vec![None; 63];
        items[0] = Some(stack("minecraft:gold_ingot", 5));
        items[62] = Some(stack("minecraft:dirt", 1)); // last hotbar slot
        assert!(menus.apply(&ClientEvent::ContainerContent {
            window_id: 5,
            state_id: 1,
            items,
            carried_item: None,
        }));
        let opened = menus.opened().expect("container open");
        assert_eq!(opened.slot_count(), 63);
        assert_eq!(
            opened.slot_item(0),
            Some(&game_stack("minecraft:gold_ingot", 5))
        );
        assert_eq!(opened.slot_item(62), Some(&game_stack("minecraft:dirt", 1)));
        assert_eq!(menus.opened_window_id(), Some(5));
        assert_eq!(menus.opened_title(), Some(&Text::literal("Chest")));
        assert_eq!(
            menus.opened_menu_type(),
            Some(&key("minecraft:generic_9x3"))
        );
    }

    #[test]
    fn container_slot_routes_by_window_id() {
        let mut menus = Menus::new();
        menus.apply(&ClientEvent::ScreenOpened {
            window_id: 5,
            menu_type: key("minecraft:generic_9x3"),
            title: Text::literal("Chest"),
        });
        menus.apply(&ClientEvent::ContainerContent {
            window_id: 5,
            state_id: 1,
            items: vec![None; 63],
            carried_item: None,
        });
        // A slot in the open container.
        assert!(menus.apply(&ClientEvent::ContainerSlot {
            window_id: 5,
            state_id: 2,
            slot: 3,
            item: Some(stack("minecraft:emerald", 2)),
        }));
        assert_eq!(
            menus.opened().unwrap().slot_item(3),
            Some(&game_stack("minecraft:emerald", 2))
        );
        // A slot in the player inventory (window 0) while the container is open.
        assert!(menus.apply(&ClientEvent::ContainerSlot {
            window_id: 0,
            state_id: 1,
            slot: 36,
            item: Some(stack("minecraft:apple", 1)),
        }));
        assert_eq!(
            menus.player().slot_item(36),
            Some(&game_stack("minecraft:apple", 1))
        );
    }

    #[test]
    fn cursor_item_targets_open_menu_then_player() {
        let mut menus = Menus::new();
        menus.apply(&ClientEvent::ScreenOpened {
            window_id: 5,
            menu_type: key("minecraft:generic_9x3"),
            title: Text::literal("Chest"),
        });
        menus.apply(&ClientEvent::ContainerContent {
            window_id: 5,
            state_id: 1,
            items: vec![None; 63],
            carried_item: None,
        });
        menus.apply(&ClientEvent::CursorItemChanged {
            item: Some(stack("minecraft:redstone", 7)),
        });
        assert_eq!(
            menus.opened().unwrap().carried(),
            Some(&game_stack("minecraft:redstone", 7))
        );

        menus.apply(&ClientEvent::ScreenClosed { window_id: 5 });
        assert!(menus.opened().is_none());
        menus.apply(&ClientEvent::CursorItemChanged {
            item: Some(stack("minecraft:coal", 3)),
        });
        assert_eq!(
            menus.player().carried(),
            Some(&game_stack("minecraft:coal", 3))
        );
    }

    #[test]
    fn screen_closed_drops_open_container() {
        let mut menus = Menus::new();
        menus.apply(&ClientEvent::ScreenOpened {
            window_id: 5,
            menu_type: key("minecraft:generic_9x3"),
            title: Text::literal("Chest"),
        });
        menus.apply(&ClientEvent::ContainerContent {
            window_id: 5,
            state_id: 1,
            items: vec![None; 63],
            carried_item: None,
        });
        assert!(menus.opened().is_some());
        assert!(menus.apply(&ClientEvent::ScreenClosed { window_id: 5 }));
        assert!(menus.opened().is_none());
        // active() falls back to the player inventory.
        assert_eq!(menus.active().slot_count(), 46);
    }

    #[test]
    fn container_data_is_readable() {
        let mut menus = Menus::new();
        menus.apply(&ClientEvent::ScreenOpened {
            window_id: 5,
            menu_type: key("minecraft:furnace"),
            title: Text::literal("Furnace"),
        });
        menus.apply(&ClientEvent::ContainerContent {
            window_id: 5,
            state_id: 1,
            items: vec![None; 39], // furnace: 3 + 36
            carried_item: None,
        });
        assert!(menus.apply(&ClientEvent::ContainerData {
            window_id: 5,
            property: 2,
            value: 180,
        }));
        assert_eq!(menus.container_data(2), Some(180));
        // Upsert overwrites.
        menus.apply(&ClientEvent::ContainerData {
            window_id: 5,
            property: 2,
            value: 200,
        });
        assert_eq!(menus.container_data(2), Some(200));
    }

    #[test]
    fn ignores_unowned_event() {
        let mut menus = Menus::new();
        assert!(!menus.apply(&ClientEvent::HealthChanged {
            health: 1.0,
            food: 1,
            saturation: 1.0,
        }));
    }
}
