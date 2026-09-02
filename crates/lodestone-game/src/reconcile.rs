//! The predict-then-reconcile seam between an optimistic client and an
//! authoritative server.
//!
//! Container interaction is **server-authoritative**. The client applies a click
//! locally the instant the player makes it — otherwise the UI would lag a full
//! round-trip — but the server is the source of truth and may disagree. Modern
//! Minecraft (1.17+) reconciles with a *container state id*: the client stamps
//! every click it sends with the id it expects, and the server echoes slot and
//! content updates. When the server's view matches the prediction the update is
//! a silent confirmation; when it differs the server's values overwrite the
//! prediction and the player sees the item "snap back".
//!
//! A design that skips this seam — applying clicks locally and assuming the
//! client is always right — works perfectly offline and desynchronises against a
//! real server the first time a click races an inventory change, a full slot, or
//! a permission the client did not model. [`ClientMenu`] keeps the two states
//! (`predicted` and `confirmed`) explicit so that divergence is observable
//! rather than silent corruption.
//!
//! This module is version-free: [`ServerUpdate`] is the canonical shape of the
//! server's container packets, which a version adapter lowers from
//! `container_set_slot` / `container_set_content` and friends.

use lodestone_model::{
    ClientAction, ContainerClickType, ContainerSlotChange, ItemStack as ModelItemStack,
};

use crate::{
    click::{Click, ClickOutcome, ContainerInput, PlayerCtx},
    container::Container,
    item::ItemStack,
    menu::Menu,
};

/// The intent to transmit for a predicted click: everything the wire
/// `container_click` packet needs, in canonical form.
///
/// A version adapter lowers this into the concrete packet, encoding
/// [`changed_slots`](Self::changed_slots) and [`carried`](Self::carried) as the
/// server expects for that protocol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClickIntent {
    /// Menu slot index (or −999 for outside).
    pub slot: i32,
    /// Raw button number.
    pub button: i32,
    /// Click mode.
    pub input: ContainerInput,
    /// The container state id the client believes is current.
    pub state_id: u32,
    /// The slots the prediction changed, as `(menu_index, new_contents)`.
    pub changed_slots: Vec<(u16, Option<ItemStack>)>,
    /// The predicted cursor contents after the click.
    pub carried: Option<ItemStack>,
    /// World-side effects (drops) the prediction produced.
    pub outcome: ClickOutcome,
}

impl ClickIntent {
    /// Lowers this intent into the canonical [`ClientAction::ContainerClick`]
    /// for `window_id`, ready for `ClientHandle::send_action`.
    ///
    /// This is the seam between the click machine and the wire, and it lives
    /// here rather than in each caller because it embeds one non-obvious rule:
    /// **the window id is the menu's, not the click's**. A click on the player's
    /// own screen goes to window `0`; a click in an open container goes to that
    /// container's id — including clicks on the player-inventory rows *inside*
    /// it, which are that container's slots, not window 0's. Sending a crafting
    /// grid click to window 0 makes the server reject the slot index outright.
    ///
    /// Component fidelity: the model's stack carries no component patch and
    /// every adapter that ships writes an empty patch for a predicted stack
    /// anyway (26.2's `HashedStack` hashes the patch, and an empty one is what
    /// the server compares against for plain items), so the lowering keeps item
    /// and count only. A stack whose components the server does track will
    /// simply hash-mismatch and be corrected, which is the reconcile seam doing
    /// its job rather than a silent desync.
    #[must_use]
    pub fn to_action(&self, window_id: i32) -> ClientAction {
        ClientAction::ContainerClick {
            window_id,
            state_id: self.state_id as i32,
            slot: self.slot,
            button: self.button,
            click_type: click_type_of(self.input),
            changed_slots: self
                .changed_slots
                .iter()
                .map(|(slot, item)| ContainerSlotChange {
                    slot: i32::from(*slot),
                    item: item.as_ref().map(to_model_stack),
                })
                .collect(),
            carried_item: self.carried.as_ref().map(to_model_stack),
        }
    }
}

/// The canonical click mode for a [`ContainerInput`]; a total 1:1 mapping, so a
/// new mode on either side is a compile error rather than a silent default.
fn click_type_of(input: ContainerInput) -> ContainerClickType {
    match input {
        ContainerInput::Pickup => ContainerClickType::Pickup,
        ContainerInput::QuickMove => ContainerClickType::QuickMove,
        ContainerInput::Swap => ContainerClickType::Swap,
        ContainerInput::Clone => ContainerClickType::Clone,
        ContainerInput::Throw => ContainerClickType::Throw,
        ContainerInput::QuickCraft => ContainerClickType::QuickCraft,
        ContainerInput::PickupAll => ContainerClickType::PickupAll,
    }
}

fn to_model_stack(stack: &ItemStack) -> ModelItemStack {
    ModelItemStack::new(
        stack.item().clone(),
        u32::try_from(stack.count()).unwrap_or(0),
    )
}

/// A server-authoritative container update, lowered from a packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerUpdate {
    /// Set one slot's contents (`container_set_slot`). A `container_id` of −1 in
    /// the wire packet targets the cursor; that is modelled as
    /// [`SetCarried`](ServerUpdate::SetCarried).
    SetSlot {
        /// The state id the server stamped this update with.
        state_id: u32,
        /// Menu slot index.
        slot: usize,
        /// New slot contents.
        item: Option<ItemStack>,
    },
    /// Replace the whole window plus the cursor (`container_set_content`).
    SetContent {
        /// The state id the server stamped this update with.
        state_id: u32,
        /// New contents for every menu slot, in order.
        items: Vec<Option<ItemStack>>,
        /// New cursor contents.
        carried: Option<ItemStack>,
    },
    /// Replace only the carried cursor stack.
    SetCarried {
        /// New cursor contents.
        item: Option<ItemStack>,
    },
}

/// The result of applying a [`ServerUpdate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reconciliation {
    /// Whether the server's values differed from the client's prediction,
    /// i.e. the player saw a correction/rollback.
    pub corrected: bool,
}

/// The one player inventory, in transit between two [`ClientMenu`]s.
///
/// A [`ClientMenu`] deliberately holds **two** [`Menu`]s — the prediction the UI
/// draws and the last thing the server confirmed — so "the one inventory" is a
/// pair here rather than a single [`Container`]. That is not the duplication
/// the single-owner-inventory fix was about: those two are the same *window*'s
/// two points in time,
/// and `reconcile` exists precisely to collapse them. That fix was about two
/// **windows** each owning a copy of the player's 41 native slots, which nothing
/// collapses.
///
/// Opaque on purpose: the only things you can do with one are take it out of a
/// menu and put it into another, which is what makes "exactly one owner" a
/// property of the type rather than of a convention.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerInventory {
    predicted: Container,
    confirmed: Container,
}

/// An optimistic client menu that predicts clicks and reconciles against server
/// updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientMenu {
    predicted: Menu,
    confirmed: Menu,
}

impl ClientMenu {
    /// Wraps a fresh menu; the initial state is taken as both predicted and
    /// confirmed.
    #[must_use]
    pub fn new(menu: Menu) -> Self {
        Self {
            confirmed: menu.clone(),
            predicted: menu,
        }
    }

    /// The predicted menu the UI should render.
    #[must_use]
    pub fn menu(&self) -> &Menu {
        &self.predicted
    }

    /// The last server-confirmed menu.
    #[must_use]
    pub fn confirmed(&self) -> &Menu {
        &self.confirmed
    }

    /// Moves the player inventory out of both menus. See
    /// [`Menu::take_player_inventory`].
    pub fn take_player_inventory(&mut self) -> PlayerInventory {
        PlayerInventory {
            predicted: self.predicted.take_player_inventory(),
            confirmed: self.confirmed.take_player_inventory(),
        }
    }

    /// Installs the player inventory into both menus. See
    /// [`Menu::take_player_inventory`].
    pub fn install_player_inventory(&mut self, inventory: PlayerInventory) {
        self.predicted.install_player_inventory(inventory.predicted);
        self.confirmed.install_player_inventory(inventory.confirmed);
    }

    /// Predicts a click locally and returns the intent to send to the server.
    ///
    /// The prediction is applied to the local menu immediately. The returned
    /// [`ClickIntent`] carries the diff and state id the server needs to
    /// reconcile.
    pub fn predict(&mut self, click: Click, ctx: PlayerCtx) -> ClickIntent {
        let before = self.predicted.snapshot();
        let outcome = click.apply(&mut self.predicted, ctx);
        let after = self.predicted.snapshot();

        let mut changed_slots = Vec::new();
        for (i, (b, a)) in before.iter().zip(after.iter()).enumerate() {
            if b != a {
                changed_slots.push((i as u16, a.clone()));
            }
        }

        ClickIntent {
            slot: click.slot,
            button: click.button,
            input: click.input,
            // **The server's** last state id, not the freshly bumped local one.
            // Vanilla's client sends `containerMenu.getStateId()` — a value only
            // ever *written* by the server (`setItem`/`initializeContents`); the
            // client never increments it (`MultiPlayerGameMode.handleContainerInput`).
            // `Menu::do_click` does bump, mirroring the server's own
            // `incrementStateId`, so `predicted.state_id()` here is always
            // `server + 1` and every single click would arrive **stale**:
            // `ServerGamePacketListenerImpl.handleContainerClick` then takes the
            // `broadcastFullState()` branch, throwing away the changed-slot
            // prediction we just computed and re-sending all 46 slots. Worse for
            // a gate than for a player: with a full resync on every click the
            // server's reply is unconditionally its own truth, so "our
            // prediction matched" becomes unfalsifiable. `confirmed` is only
            // written by `reconcile`, so its id *is* the server's.
            state_id: self.confirmed.state_id(),
            changed_slots,
            carried: self.predicted.carried().cloned(),
            outcome,
        }
    }

    /// Applies a `key.drop` removal from hotbar slot `selected` to **both** the
    /// prediction and the confirmation, returning what was removed.
    ///
    /// The semantics are entirely [`Menu::remove_from_selected`]'s — this exists
    /// only to answer *which copies* move, and the answer is unusual enough to be
    /// worth its own method rather than a `menu_mut()` accessor that would let any
    /// caller desynchronise the pair.
    ///
    /// Unlike [`predict`](Self::predict), a drop is **not** a container click: it
    /// travels as a bare `ServerboundPlayerActionPacket` and the server answers
    /// with no slot update at all, having already performed the identical removal
    /// on its own inventory. So `confirmed` is not "what the server last told us"
    /// here but "what we know the server did", and it has to follow `predicted`
    /// or the next full `container_set_content` diffs as a visible correction
    /// that never happened. See [`crate::menus::Menus::drop_selected`].
    pub fn remove_from_selected(&mut self, selected: usize, all: bool) -> Option<ItemStack> {
        let removed = self.predicted.remove_from_selected(selected, all)?;
        let remainder = self.predicted.player_native(selected).cloned();
        self.confirmed.set_player_native(selected, remainder);
        Some(removed)
    }

    /// Applies a server update, overwriting the prediction where the server
    /// disagrees. Returns whether a visible correction occurred.
    pub fn reconcile(&mut self, update: ServerUpdate) -> Reconciliation {
        match update {
            ServerUpdate::SetSlot {
                state_id,
                slot,
                item,
            } => {
                self.confirmed.set_slot_item(slot, item.clone());
                let diverged = self.predicted.slot_item_cloned(slot) != item;
                if diverged {
                    self.predicted.set_slot_item(slot, item);
                }
                self.sync_state_id(state_id);
                Reconciliation {
                    corrected: diverged,
                }
            }
            ServerUpdate::SetContent {
                state_id,
                items,
                carried,
            } => {
                self.confirmed.restore(&items);
                self.confirmed.set_carried(carried.clone());
                let diverged = self.predicted.snapshot() != items
                    || self.predicted.carried().cloned() != carried;
                if diverged {
                    self.predicted.restore(&items);
                    self.predicted.set_carried(carried);
                }
                self.sync_state_id(state_id);
                Reconciliation {
                    corrected: diverged,
                }
            }
            ServerUpdate::SetCarried { item } => {
                self.confirmed.set_carried(item.clone());
                let diverged = self.predicted.carried().cloned() != item;
                if diverged {
                    self.predicted.set_carried(item);
                }
                Reconciliation {
                    corrected: diverged,
                }
            }
        }
    }

    /// Forces the state ids of both menus to the server's, so the next
    /// predicted click stamps the id the server expects.
    fn sync_state_id(&mut self, state_id: u32) {
        self.predicted.set_state_id(state_id);
        self.confirmed.set_state_id(state_id);
    }

    /// Applies a direct server set of a player-inventory slot addressed by
    /// *native* index (the `set_player_inventory` packet), overwriting both the
    /// prediction and the confirmation. Returns whether it diverged from the
    /// prediction.
    ///
    /// Native indexing (`0..=8` hotbar, `9..=35` main, `36..=39` armour, `40`
    /// off-hand) is a **different** numbering from the window-0 menu order, so
    /// this must not be routed through [`reconcile`](Self::reconcile)'s
    /// menu-indexed [`SetSlot`](ServerUpdate::SetSlot) — doing so would transpose
    /// hotbar and crafting/armour slots.
    pub fn set_player_native(&mut self, native_index: usize, item: Option<ItemStack>) -> bool {
        let diverged = self.predicted.player_native(native_index).cloned() != item;
        self.confirmed.set_player_native(native_index, item.clone());
        self.predicted.set_player_native(native_index, item);
        diverged
    }
}
