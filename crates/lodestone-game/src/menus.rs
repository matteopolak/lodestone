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

use lodestone_model::{
    ClientAction, ClientEvent, ItemStack as ModelItemStack, Text, ids::ResourceKey,
};

use crate::{
    click::{Click, PlayerCtx},
    item::ItemStack,
    menu::{Menu, SpecialLayout},
    recipe::{CraftingGrid, RecipeBook},
    reconcile::{ClickIntent, ClientMenu, ServerUpdate},
};

/// The player-inventory portion (main + hotbar) appended to every non-player
/// `container_set_content`. Armour and off-hand are *not* included for open
/// containers, so a generic container's content length is `container_size + 36`.
const PLAYER_INVENTORY_PORTION: usize = 36;

/// The window id every **plugin-opened**, purely local menu carries.
///
/// `i32::MIN`, and the value matters. A local menu has no server-side container,
/// so its id must be one a server can never legitimately allocate *and* one that
/// is obviously wrong if it ever escapes onto the wire — vanilla window ids are
/// small positives (`0` is the player's own inventory). Picking an unused small
/// negative would have been indistinguishable from a protocol quirk in a packet
/// log; this cannot be mistaken for anything.
///
/// Consumers must not branch on this value directly. Ask
/// [`Menus::opened_is_local`], which is the authority — the id is a belt to that
/// braces.
pub const LOCAL_MENU_WINDOW_ID: i32 = i32::MIN;

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
    /// Whether this screen was opened by a plugin rather than by a server
    /// `open_screen`.
    ///
    /// The authority for "must nothing about this reach the wire". A `bool`
    /// rather than deriving it from `window_id` so the two can be cross-checked,
    /// and so a future multi-local-menu change has somewhere to put a real id.
    local: bool,
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
///
/// # One inventory, one owner
///
/// Vanilla has a single `Inventory`; the player-inventory menu's slots and every
/// container menu's player-section slots are all references into it
/// **references into it**, which is why a shift-click inside a crafting table
/// updates the HUD hotbar for free. Two owned [`Menu`]s cannot share a
/// `Container` in Rust, so the aliasing is modelled as ownership that *moves*:
///
/// * [`player`](Self::player) owns the inventory whenever nothing is open;
/// * opening a container hands it to that container's menu
///   ([`hand_inventory_to_opened`](Self::hand_inventory_to_opened));
/// * closing (or replacing) it hands it back
///   ([`reclaim_inventory`](Self::reclaim_inventory)).
///
/// The invariant is that **at no instant do two copies of the player's 41 native
/// slots exist inside a `Menus`** — so there is nothing to synchronise and
/// nothing that can diverge. Before this, the HUD read window 0's copy while
/// every quick-move mutated the container's copy: the item was usable (the
/// server had it) and the hotbar cell stayed blank.
///
/// Two consequences worth knowing before touching this type:
///
/// * [`player`](Self::player) hands out a **clone with the live inventory
///   installed**, not a borrow of the window-0 menu, because that menu's player
///   section is an empty husk while a container is open.
/// * a window-0 `container_set_slot`/`container_set_content` addressed at the
///   player section has to be forwarded to the current owner
///   ([`forward_window_zero_slot`](Self::forward_window_zero_slot)); vanilla gets
///   this for free through the reference.
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

    /// The player's own inventory menu (window 0), always present, **with the
    /// live inventory in it**.
    ///
    /// # Why this returns a value and not a `&Menu`
    ///
    /// There is one player inventory and it has one owner (see
    /// [`Menu::take_player_inventory`]). While a container is open that owner is
    /// the *container's* menu, so `&self.player.menu()` would hand out a window-0
    /// menu whose player section is an empty husk. Returning a clone lets this
    /// reinstall the live inventory, so a caller cannot obtain a stale — or
    /// blank — hotbar no matter which screen is up. This is exactly the
    /// aliasing bug this menu's own single-ownership model exists to close:
    /// the HUD
    /// read window 0's copy, a quick-move mutated the container's copy, and the
    /// row never changed.
    ///
    /// Both existing callers ([`crate::menus`]'s two shell consumers,
    /// `Sim::player_menu` and `SharedState::player_menu`) already cloned the
    /// result, so this costs nothing over the old shape.
    #[must_use]
    pub fn player(&self) -> Menu {
        let mut menu = self.player.menu().clone();
        if let Some(open) = &self.opened {
            menu.install_player_inventory(open.menu.menu().player_inventory().clone());
        }
        menu
    }

    /// One slot of the one player inventory, by **native** index (`0..=8`
    /// hotbar, `9..=35` main, `36..=39` armour, `40` off-hand), read from
    /// whichever menu currently owns it.
    ///
    /// The borrow-friendly counterpart to [`player`](Self::player) for callers
    /// that want a stack rather than a screen — the HUD's held item, the
    /// mining-speed tool lookup. Use this in preference to cloning a whole menu
    /// to read one slot.
    #[must_use]
    pub fn player_native(&self, native_index: usize) -> Option<&ItemStack> {
        match &self.opened {
            Some(open) => open.menu.menu().player_native(native_index),
            None => self.player.menu().player_native(native_index),
        }
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

    /// Every `container_set_data` property of the open container, as
    /// `(property_id, value)` — the borrow-friendly counterpart to
    /// [`Self::container_data`] for a caller that wants the whole set rather
    /// than probing one property at a time (the anvil's single cost slot vs.
    /// the enchanting table's three). Empty when nothing is open or the
    /// server has sent no properties yet, never a stale value from a
    /// previously open screen — `ScreenOpened`'s own fold starts a fresh
    /// `OpenMenu` with an empty `data`, so there is nothing here to carry
    /// over.
    #[must_use]
    pub fn opened_data(&self) -> &[(i32, i32)] {
        self.opened.as_ref().map_or(&[], |o| o.data.as_slice())
    }

    /// Whether the open screen was opened by a plugin rather than by the server.
    ///
    /// **Every wire-facing consumer must check this before sending anything about
    /// the open menu.** A local menu has no server-side container, so a
    /// `ContainerClose` or `ContainerClick` naming its window id is addressed to
    /// something that does not exist. `false` when nothing is open, which is the
    /// safe answer for a caller that forgot to check whether one was.
    #[must_use]
    pub fn opened_is_local(&self) -> bool {
        self.opened.as_ref().is_some_and(|o| o.local)
    }

    /// Opens a **local** menu — one a plugin supplied, with no server container
    /// behind it. `Bukkit.createInventory` + `Player.openInventory`.
    ///
    /// # Why this cannot go through `apply`
    ///
    /// Pushing a synthetic `ScreenOpened` + `ContainerContent` pair was the only
    /// route before this method, and it does draw — but it is wrong in three ways
    /// that are invisible until they bite:
    ///
    /// 1. `ScreenOpened` alone opens **nothing**. The menu is not built until a
    ///    `ContainerContent` arrives, because the container's *size* comes from
    ///    that packet's item-count minus 36. A plugin pushing only the open event
    ///    gets no screen and no error.
    /// 2. The synthetic content packet's length is what sizes the menu, and
    ///    `build_menu` re-derives the **layout** from the menu-type key. So a
    ///    plugin could not supply a pre-built [`Menu`] at all — only a key and a
    ///    length, and any key outside `build_menu`'s table silently became
    ///    `Menu::generic`.
    /// 3. The result is indistinguishable from a server open in every downstream
    ///    consumer, so `ContainerClose` and every `ContainerClick` went to the
    ///    real server naming a window it had never heard of.
    ///
    /// This method takes the `Menu` the plugin actually built — any of
    /// [`Menu`]'s constructors, including the `SpecialLayout` ones — and marks the
    /// screen local so (3) cannot happen.
    ///
    /// # It takes the player inventory, like every other open
    ///
    /// The single-owner invariant above holds for local menus too: the one player inventory
    /// moves into this menu while it is open and is reclaimed on close. That is
    /// what makes the 27 + 9 rows drawn underneath a plugin's screen the real
    /// inventory rather than an empty husk, and what makes shift-clicking out of a
    /// plugin menu land in the hotbar.
    pub fn open_local(&mut self, menu: Menu, menu_type: ResourceKey, title: Text) {
        // Whatever was open is holding the one player inventory; take it back
        // before dropping that menu, exactly as `ensure_open` does.
        self.reclaim_inventory();
        // A pending server open is *not* cleared: if its content packet arrives
        // it legitimately supersedes this local menu, and dropping the pending
        // here would strand that screen with unknown metadata forever.
        self.opened = Some(OpenMenu {
            window_id: LOCAL_MENU_WINDOW_ID,
            menu_type: Some(menu_type),
            title: Some(title),
            menu: ClientMenu::new(menu),
            data: Vec::new(),
            local: true,
        });
        self.hand_inventory_to_opened();
    }

    /// Closes the open menu **only if it is local**, returning whether it closed.
    ///
    /// Deliberately narrower than `apply(ScreenClosed)`: a plugin closing its own
    /// screen must not be able to close a real server container behind the
    /// player's back, because that would desynchronise the server's own open
    /// container with no packet explaining why.
    pub fn close_local(&mut self) -> bool {
        if !self.opened_is_local() {
            return false;
        }
        self.reclaim_inventory();
        self.opened = None;
        true
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
                    // Before the menu goes: take the one inventory back.
                    self.reclaim_inventory();
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
                    // Same re-addressing as the single-slot case, for every
                    // player-section slot this content packet just wrote.
                    if self.opened.is_some() {
                        for slot in 0..self.player.menu().slot_count() {
                            self.forward_window_zero_slot(slot);
                        }
                    }
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
                if let Ok(slot) = usize::try_from(*slot) {
                    if let Some(menu) = self.menu_for_mut(*window_id) {
                        menu.reconcile(ServerUpdate::SetSlot {
                            state_id: *state_id as u32,
                            slot,
                            item: model_item_to_game(item),
                        });
                    }
                    if *window_id == 0 {
                        self.forward_window_zero_slot(slot);
                    }
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
                    // Vanilla's container id `-2` writes straight into the one
                    // `Inventory` (`handleContainerSetSlot`'s `-2` arm), so this
                    // goes to whichever menu owns it, not unconditionally to
                    // window 0.
                    self.inventory_owner_mut()
                        .set_player_native(native, model_item_to_game(item));
                }
            }
            _ => return false,
        }
        true
    }

    /// Hands the one player inventory to the open container's menu, so its
    /// player rows and the HUD hotbar are the **same storage**.
    ///
    /// Called exactly once per container open, from [`ensure_open`](Self::ensure_open),
    /// *before* the server's `container_set_content` is reconciled into it — so
    /// the content packet's own 36 player slots land in the inventory that is
    /// now there, not in a container that is about to be replaced.
    fn hand_inventory_to_opened(&mut self) {
        let inventory = self.player.take_player_inventory();
        if let Some(open) = self.opened.as_mut() {
            open.menu.install_player_inventory(inventory);
        } else {
            // Nothing to hand it to; put it straight back rather than drop it.
            self.player.install_player_inventory(inventory);
        }
    }

    /// Takes the player inventory back out of the open container's menu before
    /// that menu is dropped or replaced. The counterpart to
    /// [`hand_inventory_to_opened`](Self::hand_inventory_to_opened).
    ///
    /// **This is what makes the hotbar right after the screen closes.** A vanilla
    /// server sends nothing on close (`ServerPlayer.doCloseContainer` only calls
    /// `transferState`), so anything the player rearranged inside the container
    /// would be lost on close if the storage went out with the menu.
    fn reclaim_inventory(&mut self) {
        if let Some(open) = self.opened.as_mut() {
            let inventory = open.menu.take_player_inventory();
            self.player.install_player_inventory(inventory);
        }
    }

    /// Re-addresses a **window-0** menu slot the server just wrote into
    /// `self.player` onto whichever menu currently owns the player inventory.
    ///
    /// Window 0's player-section slots address the one inventory — vanilla's
    /// `ClientPacketListener.handleContainerSetSlot` routes container id `0` to
    /// `player.inventoryMenu`, whose slots reference the shared `Inventory`, so
    /// a window-0 update lands in the same storage an open chest is showing.
    /// Here, while a container is open, `self.player`'s player container is a
    /// husk, so the value has to be forwarded. Slots 0..5 (the 2×2 grid and its
    /// result) belong to window 0's *own* containers and are already correct.
    ///
    /// Reads the value back out of the husk rather than taking it as an
    /// argument, so the menu-slot → native-index mapping comes from
    /// [`Menu::slot_native`] — the same `Slot` table the draw walks — and not
    /// from a second transcription of the window-0 layout.
    fn forward_window_zero_slot(&mut self, menu_slot: usize) {
        if self.opened.is_none() {
            return;
        }
        let Some(native) = self.player.menu().slot_native(menu_slot) else {
            return;
        };
        let item = self.player.menu().player_native(native).cloned();
        if let Some(open) = self.opened.as_mut() {
            open.menu.set_player_native(native, item);
        }
    }

    /// The [`ClientMenu`] that currently owns the player inventory.
    fn inventory_owner_mut(&mut self) -> &mut ClientMenu {
        match &mut self.opened {
            Some(open) => &mut open.menu,
            None => &mut self.player,
        }
    }

    /// Returns the [`ClientMenu`] for a window id, or `None` if the id is neither
    /// the player inventory nor the open container.
    fn menu_for_mut(&mut self, window_id: i32) -> Option<&mut ClientMenu> {
        if window_id == 0 {
            return Some(&mut self.player);
        }
        self.opened
            .as_mut()
            // `!o.local` is belt-and-braces against the id check: a local menu's
            // id is `i32::MIN`, which no server allocates, but a server-sourced
            // packet must be unable to write into a plugin's screen even if one
            // somehow arrived with that id.
            .filter(|o| !o.local && o.window_id == window_id)
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
            // A different window replacing this one (or an open with no close):
            // the outgoing menu is holding the one player inventory.
            self.reclaim_inventory();
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
            // `LecternMenu` exposes only its single displayed-book slot. It
            // does not append the normal 36 player-inventory slots to its
            // content packet, despite borrowing the player's inventory for
            // interaction. Keep that one slot in the model so the shell can
            // open the book reader rather than drawing a zero-slot chest.
            let container_size = if menu_type.as_ref().is_some_and(|key| {
                key.namespace() == "minecraft" && key.path() == "lectern"
            }) {
                content_len
            } else {
                content_len.saturating_sub(PLAYER_INVENTORY_PORTION)
            };
            let menu = build_menu(menu_type.as_ref(), container_size);
            self.opened = Some(OpenMenu {
                window_id,
                menu_type,
                title,
                menu: ClientMenu::new(menu),
                data: Vec::new(),
                // Reached only from a server `container_set_content`, so never
                // local. A server open legitimately *supersedes* a plugin's local
                // menu: `reclaim_inventory` above has already taken the one player
                // inventory back out of it.
                local: false,
            });
            // Vanilla's shape: the container's player-section slots *are* the
            // player inventory. Hand it over before the caller reconciles the
            // content packet into this menu.
            self.hand_inventory_to_opened();
        }
        &mut self.opened.as_mut().expect("just set").menu
    }

    /// Predicts `click` on the **active** menu and returns the window id it must
    /// be addressed to together with the intent to transmit.
    ///
    /// This is the serverbound half of the session, and the counterpart to
    /// [`apply`](Self::apply): the UI turns a mouse event into a [`Click`], this
    /// applies it optimistically so the screen responds immediately, and the
    /// returned [`ClickIntent`] — lowered by
    /// [`ClickIntent::to_action`] — is what goes on the wire. Whatever the
    /// server thinks of it comes back through `apply` and overwrites the
    /// prediction.
    ///
    /// The window id is the **active menu's**, and that is the whole reason this
    /// returns it rather than leaving the caller to guess: while a container is
    /// open, *every* slot on screen belongs to that container's window —
    /// including the 27 + 9 player-inventory rows drawn underneath it. Sending a
    /// click on those rows to window `0` addresses a completely different slot
    /// list.
    ///
    /// Nothing here matches a recipe. A click into a crafting grid is an
    /// ordinary slot move; the result slot is filled by the server's
    /// `container_set_slot`, which [`apply`](Self::apply) already reconciles.
    pub fn click(&mut self, click: Click, ctx: PlayerCtx) -> (i32, ClickIntent) {
        match &mut self.opened {
            Some(open) => {
                let window_id = open.window_id;
                (window_id, open.menu.predict(click, ctx))
            }
            None => (0, self.player.predict(click, ctx)),
        }
    }

    /// The [`ClientAction`] for a predicted click on the active menu: [`click`]
    /// followed by [`ClickIntent::to_action`], which is the whole serverbound
    /// path in one call for a UI that needs nothing else from the intent.
    ///
    /// [`click`]: Self::click
    pub fn click_action(&mut self, click: Click, ctx: PlayerCtx) -> ClientAction {
        let (window_id, intent) = self.click(click, ctx);
        intent.to_action(window_id)
    }

    /// Predicts a `key.drop` press in normal gameplay: removes from hotbar slot
    /// `selected` (`0..9`) and returns what was dropped, or `None` if the slot was
    /// empty.
    ///
    /// `all == false` is plain `Q` (one item), `all == true` is `Ctrl`+`Q` (the
    /// whole stack) — the same fork
    /// [`ClientAction::DropSelectedItem`](lodestone_model::ClientAction::DropSelectedItem)
    /// / `DropSelectedItemStack` already carries on the wire. Semantics are
    /// [`Menu::remove_from_selected`]'s; what this layer adds is *which copy* gets
    /// mutated.
    ///
    /// # Why this is not a container click, and why it writes both copies
    ///
    /// Drop is **not** a `ClickType::THROW`. It travels as a bare
    /// `ServerboundPlayerActionPacket` with no window id, no slot and no state
    /// id, so there is nothing for [`ClientMenu::reconcile`] to correct against
    /// and no `state_id` to bump — going through [`Self::click`] would fabricate
    /// a container-click round trip the server never sees.
    ///
    /// It therefore writes **`predicted` and `confirmed` alike**, via
    /// [`ClientMenu::set_player_native`], because the server performs the *same*
    /// mutation on its own inventory without telling us
    /// (vanilla's own server-side drop-item packet handler calls `player.drop(…)`, then
    /// returns without a reply). Predicting only into `predicted` would leave `confirmed`
    /// permanently one item richer than the server, so the next full
    /// `container_set_content` would diff as a *visible correction* that never
    /// actually happened. Prediction here means "we know what the server did",
    /// not "we are guessing ahead of a reply".
    ///
    /// Routing goes through [`Self::inventory_owner_mut`] for the same single-owner
    /// reason: while a container screen is open the one player inventory is owned
    /// by the *container's* menu and window 0's copy is an empty husk, so writing
    /// `self.player` would land the removal in a menu nothing draws.
    pub fn drop_selected(&mut self, selected: usize, all: bool) -> Option<ItemStack> {
        self.inventory_owner_mut()
            .remove_from_selected(selected, all)
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
///
/// # Why everything else is `generic`, and what that costs
///
/// Vanilla overrides its own quick-move step per menu class, so in principle every
/// `minecraft:menu` registry entry could need its own arm. In practice most of
/// them are the *same* override with a different constant:
/// vanilla's own chest, hopper, dispenser
/// and shulker-box quick-move steps are line-for-line identical modulo the
/// container size, which is exactly what [`Menu::generic`] implements. That one
/// arm correctly covers chests, barrels, ender chests, every `generic_9xN`,
/// hoppers, dispensers, droppers and shulker boxes.
///
/// Two families are genuinely different and are **knowingly** left on the
/// generic order:
///
/// * Vanilla's own furnace-family quick-move step routes by *item kind*: smeltables to slot
///   0, fuel to slot 1, and only otherwise the main↔hotbar hop. Both predicates
///   (`canSmelt` → the cooking-recipe input set, `isFuel` → the fuel-value
///   registry) are server data this tree does not have. Modelling the structure
///   without them would just move the guess.
/// * Vanilla's own brewing-stand quick-move step does the same for blaze powder, brewing
///   ingredients and potions.
///
/// The cost is bounded and self-correcting: a shift-click in a furnace predicts
/// a deposit into container slot 0 where vanilla would have chosen slot 1 or
/// done nothing, the server disagrees, and
/// [`ClientMenu::reconcile`](crate::reconcile::ClientMenu::reconcile) snaps it
/// back one round trip later. It is a visible flicker, not a desync.
///
/// Adding a case must **not** grow [`MenuKind`](crate::menu::MenuKind): that
/// enum is matched exhaustively in `lodestone-shell`'s `slot_layout`, and the
/// crafting table was deliberately kept a `Generic` for exactly that reason.
/// Carry the extra routing as a descriptor on [`Menu`], the way
/// [`CraftLayout`](crate::menu::CraftLayout) already is.
///
/// The anvil, grindstone, smithing table and enchanting table
/// are four more cases of exactly this: [`Menu::item_combiner`]
/// and [`Menu::enchanting_table`] both still build on [`Menu::generic`] and
/// stay `MenuKind::Generic`. `container_size` is checked alongside
/// `menu_type` for the same reason [`is_crafting`] checks `10`: if the server
/// ever disagrees about the size, falling back to a plain generic container
/// (whose slot count *does* match the packet) beats building a menu that
/// contradicts what was actually sent.
///
/// A later change ("container screens: the whole family") added the furnace
/// family, the brewing stand, the loom, the stonecutter, the cartography
/// table and the dispenser/dropper on top of that same pattern —
/// [`Menu::furnace`], [`Menu::brewing_stand`], [`Menu::loom`],
/// [`Menu::stonecutter`], [`Menu::cartography_table`] and
/// [`Menu::dispenser`] all still build on [`Menu::generic`] and stay
/// `MenuKind::Generic`, attaching only a [`SpecialLayout`] for
/// `lodestone-shell` to draw the right panel and slot positions. Two of
/// these screens have a real button-driven sub-feature this pass does not
/// model — the loom's pattern grid and the stonecutter's recipe list —
/// because each needs data this tree does not carry yet (a
/// banner-pattern/recipe registry). The slots themselves need none of that:
/// they are the same "accept anything, let the server's `container_set_slot`
/// correct a wrong guess" order already established above.
///
/// The beacon (also part of that same family) is no longer in that list:
/// [`Menu::beacon`] builds the real `BeaconMenu` shape, and its
/// primary/secondary power buttons and confirm/cancel controls — not menu
/// slots, driven off `container_data` and screen-local selection state — are
/// `lodestone-shell`'s `container::beacon` module's job (the
/// `SetBeaconEffects` remainder).
///
/// The villager's trade list is no longer in that list: [`Menu::merchant`]
/// builds the real `MerchantMenu` shape, and the trade
/// *offers* themselves — the seven-row scrollable list, not menu slots at all
/// — arrive separately as [`crate::trades::TradeOffers`] and are drawn by
/// `lodestone_shell::container::merchant`.
fn build_menu(menu_type: Option<&ResourceKey>, container_size: usize) -> Menu {
    let is_crafting =
        menu_type.is_some_and(|key| key.namespace() == "minecraft" && key.path() == "crafting");
    if is_crafting && container_size == 10 {
        return Menu::crafting(3, 3);
    }
    let path = menu_type
        .filter(|key| key.namespace() == "minecraft")
        .map(ResourceKey::path);
    match (path, container_size) {
        (Some("anvil"), 3) => Menu::item_combiner(3, 2, SpecialLayout::Anvil),
        (Some("grindstone"), 3) => Menu::item_combiner(3, 2, SpecialLayout::Grindstone),
        (Some("smithing"), 4) => Menu::item_combiner(4, 3, SpecialLayout::Smithing),
        (Some("enchantment"), 2) => Menu::enchanting_table(),
        // The furnace family: three menu types, one shape.
        // `Menu::furnace` takes the layout so the background art (the only
        // thing that differs between them) can still be told apart.
        (Some("furnace"), 3) => Menu::furnace(SpecialLayout::Furnace),
        (Some("blast_furnace"), 3) => Menu::furnace(SpecialLayout::BlastFurnace),
        (Some("smoker"), 3) => Menu::furnace(SpecialLayout::Smoker),
        (Some("brewing_stand"), 5) => Menu::brewing_stand(),
        (Some("loom"), 4) => Menu::loom(),
        (Some("stonecutter"), 2) => Menu::stonecutter(),
        (Some("cartography_table"), 3) => Menu::cartography_table(),
        // Shared by the dispenser and the dropper — see
        // `SpecialLayout::Dispenser`'s doc comment for why there is no
        // separate dropper case.
        (Some("generic_3x3"), 9) => Menu::dispenser(),
        // Not documented among the same named containers as the furnace
        // family — found while documenting
        // it (see `SpecialLayout::Hopper`'s doc comment).
        (Some("hopper"), 5) => Menu::hopper(),
        // The merchant/trading screen. `container_size == 3`
        // matches `MerchantMenu`'s two payment slots plus its take-only
        // result — see `Menu::merchant`'s doc comment for what is and is not
        // modelled.
        (Some("merchant"), 3) => Menu::merchant(),
        // The beacon screen (the `SetBeaconEffects` remainder).
        // `container_size == 1` matches `BeaconMenu`'s one payment slot
        // (vanilla's own beacon slot-count constant).
        (Some("beacon"), 1) => Menu::beacon(),
        _ => Menu::generic(container_size),
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

    /// A lectern has exactly one menu slot: the displayed book. Unlike every
    /// ordinary container it does **not** append the player's 36 inventory
    /// slots to `container_set_content`, so treating `items.len() - 36` as its
    /// container size turns it into a zero-slot generic chest and loses the
    /// book before the shell can open its reader.
    #[test]
    fn lectern_content_keeps_its_book_in_menu_slot_zero() {
        let mut menus = Menus::new();
        let book = stack("minecraft:written_book", 1);
        assert!(menus.apply(&ClientEvent::ScreenOpened {
            window_id: 7,
            menu_type: key("minecraft:lectern"),
            title: Text::literal("Lectern"),
        }));
        assert!(menus.apply(&ClientEvent::ContainerContent {
            window_id: 7,
            state_id: 0,
            items: vec![Some(book.clone())],
            carried_item: None,
        }));

        let opened = menus.opened().expect("lectern must open from its content");
        assert_eq!(opened.slot_item(0), Some(&GameItemStack::from(&book)));
        assert_eq!(opened.slot_count(), 37, "one lectern slot plus player inventory");
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

    /// `build_menu` must key the anvil, grindstone, smithing table
    /// and enchanting table off the wire `menu_type`, not just fall through to
    /// a plain [`Menu::generic`] the way every other unmodelled screen still
    /// does. Checked through the real `ScreenOpened` → `ContainerContent`
    /// path, not by calling the constructors directly, so this also proves
    /// `build_menu`'s dispatch — not just the constructors it dispatches to.
    #[test]
    fn build_menu_selects_the_item_combiner_shape_for_anvil_grindstone_and_smithing() {
        let open = |menu_type: &str, container_size: usize| {
            let mut menus = Menus::new();
            assert!(menus.apply(&ClientEvent::ScreenOpened {
                window_id: 5,
                menu_type: key(menu_type),
                title: Text::literal("T"),
            }));
            assert!(menus.apply(&ClientEvent::ContainerContent {
                window_id: 5,
                state_id: 1,
                items: vec![None; container_size + 36],
                carried_item: None,
            }));
            menus
        };

        for menu_type in ["minecraft:anvil", "minecraft:grindstone"] {
            let menus = open(menu_type, 3);
            let opened = menus.opened().expect("container open");
            assert!(
                !opened.may_place(2, &game_stack("minecraft:diamond", 1)),
                "{menu_type}'s result slot (index 2) must be take-only"
            );
            assert!(opened.may_place(0, &game_stack("minecraft:diamond", 1)));
        }

        let smithing = open("minecraft:smithing", 4);
        let opened = smithing.opened().expect("container open");
        assert!(
            !opened.may_place(3, &game_stack("minecraft:diamond", 1)),
            "smithing's result slot (index 3) must be take-only"
        );

        let enchanting = open("minecraft:enchantment", 2);
        let opened = enchanting.opened().expect("container open");
        assert!(
            !opened.may_place(1, &game_stack("minecraft:diamond", 1)),
            "the enchanting table's slot 1 must reject a non-lapis item"
        );
        assert!(opened.may_place(1, &game_stack("minecraft:lapis_lazuli", 1)));
    }

    /// Control for the test above: a size the server never actually sends for
    /// an anvil (real anvils are always 3 slots) must **not** get the
    /// item-combiner treatment — `build_menu` falls back to a plain generic
    /// container, matching the crafting-table precedent this dispatch was
    /// modelled on. Without this, the gate above could pass merely because
    /// `menu_type == "anvil"` was enough on its own, regardless of size.
    #[test]
    fn control_anvil_menu_type_with_the_wrong_size_falls_back_to_generic() {
        let mut menus = Menus::new();
        assert!(menus.apply(&ClientEvent::ScreenOpened {
            window_id: 5,
            menu_type: key("minecraft:anvil"),
            title: Text::literal("T"),
        }));
        assert!(menus.apply(&ClientEvent::ContainerContent {
            window_id: 5,
            state_id: 1,
            items: vec![None; 9 + 36], // not the real 3-slot anvil size
            carried_item: None,
        }));
        let opened = menus.opened().expect("container open");
        assert_eq!(opened.slot_count(), 45);
        assert!(
            opened.may_place(2, &game_stack("minecraft:diamond", 1)),
            "a mismatched size must not get the take-only result slot"
        );
    }

    /// `build_menu` must select [`Menu::merchant`] for a real
    /// `minecraft:merchant` open, checked through the
    /// same `ScreenOpened` -> `ContainerContent` path as the item-combiner
    /// screens above — not by calling `Menu::merchant` directly, so this
    /// proves the *dispatch*, not just the constructor. This is the "real
    /// path" half of the merchant screen's island control: a client that
    /// never routed `minecraft:merchant` to the real shape would still pass
    /// every one of `Menu::merchant`'s own unit tests.
    #[test]
    fn build_menu_selects_the_merchant_shape_for_a_real_open() {
        let mut menus = Menus::new();
        assert!(menus.apply(&ClientEvent::ScreenOpened {
            window_id: 7,
            menu_type: key("minecraft:merchant"),
            title: Text::literal("Villager"),
        }));
        assert!(menus.apply(&ClientEvent::ContainerContent {
            window_id: 7,
            state_id: 1,
            items: vec![None; 3 + 36],
            carried_item: None,
        }));
        let opened = menus.opened().expect("container open");
        assert_eq!(
            opened.special_layout(),
            Some(SpecialLayout::Merchant),
            "a real minecraft:merchant open must build the merchant shape, \
             not fall through to a plain generic container"
        );
        assert!(
            !opened.may_place(2, &game_stack("minecraft:diamond", 1)),
            "the merchant's result slot (index 2) must be take-only"
        );
        assert!(
            opened.may_place(0, &game_stack("minecraft:emerald", 1)),
            "the merchant's payment slots must accept anything (no server \
             predicate data to check against client-side)"
        );
    }

    /// Control for the test above: a size the server never actually sends
    /// for a merchant (real `MerchantMenu`s are always 3 container slots)
    /// must **not** get the merchant shape — `build_menu` falls back to a
    /// plain generic container, the same size-guard the anvil/grindstone/
    /// smithing control above exercises.
    #[test]
    fn control_merchant_menu_type_with_the_wrong_size_falls_back_to_generic() {
        let mut menus = Menus::new();
        assert!(menus.apply(&ClientEvent::ScreenOpened {
            window_id: 7,
            menu_type: key("minecraft:merchant"),
            title: Text::literal("Villager"),
        }));
        assert!(menus.apply(&ClientEvent::ContainerContent {
            window_id: 7,
            state_id: 1,
            items: vec![None; 9 + 36], // not the real 3-slot merchant size
            carried_item: None,
        }));
        let opened = menus.opened().expect("container open");
        assert_eq!(opened.slot_count(), 45);
        assert_eq!(opened.special_layout(), None);
        assert!(
            opened.may_place(2, &game_stack("minecraft:diamond", 1)),
            "a mismatched size must not get the take-only result slot"
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
