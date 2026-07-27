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

use crate::{
    click::{Click, ClickOutcome, ContainerInput, PlayerCtx},
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
            state_id: self.predicted.state_id(),
            changed_slots,
            carried: self.predicted.carried().cloned(),
            outcome,
        }
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
}
