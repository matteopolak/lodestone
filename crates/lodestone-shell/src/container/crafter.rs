//! The crafter's slot-disable toggle — `SetContainerSlotState`
//! remainder, the client (producer) half.
//!
//! ## What it is
//!
//! `CrafterScreen.slotClicked` (`.cache/mc/26.2/client-src`) lets a plain
//! click on an empty, non-spectator crafter slot toggle whether that slot
//! participates in crafting: re-enable a disabled slot unconditionally, or
//! disable an enabled one only when the cursor is carrying nothing (so a
//! normal item placement onto an enabled empty slot still works).
//!
//! ## How it works
//!
//! [`toggle_decision`] is that gate, pulled out as a pure function so it can
//! be unit-tested without a live `WindowApp`/`Menu` fixture — the WindowApp
//! glue (`app::container_input::WindowApp::maybe_toggle_crafter_slot`)
//! resolves `disabled`/`carried_present` from the real `Menu`/
//! `OpenMenuSnapshot` and just calls this.
//!
//! Two things this module does **not** decide, left to the call site because
//! they need the real `Menu`/`OpenMenuSnapshot`, not just booleans:
//!
//! * Whether the click even lands on a crafter's own craft-grid slot (the
//!   `menu_type == "crafter_3x3"` and `index < 9` checks).
//! * Whether the click *consumes* the input event — vanilla's own override
//!   never does (`super.slotClicked` still runs right after), so the caller
//!   must not treat this as a first-refusal widget the way the beacon/
//!   enchant/loom/stonecutter click handlers are.
//!
//! ## How to change it
//!
//! There is no local, optimistic `containerData` mutation the way
//! `CrafterMenu.setSlotState` gives vanilla's own client — `disabled` is
//! read fresh from [`lodestone_client::OpenMenuSnapshot::data`] on every
//! click, so the toggle only becomes visible once the server's
//! `container_set_data` echoes it back. Vanilla's `SWAP` case (a hotbar
//! number pressed over a disabled slot holding a matching item) is not
//! modelled at all — only `PICKUP`.
//!
//! ## Configuration
//!
//! None.
//!
//! ## Dependencies
//!
//! None — pure arithmetic over booleans.

/// `CrafterScreen.slotClicked`'s `PICKUP` case, as a pure decision: given
/// whether the slot is currently disabled and whether the cursor is
/// carrying an item, what new state (if any) to send.
///
/// Returns `None` when vanilla's own switch falls through with no send at
/// all — carrying an item over an *enabled* slot, which must fall through to
/// an ordinary item placement instead.
#[must_use]
pub fn toggle_decision(disabled: bool, carried_present: bool) -> Option<bool> {
    if disabled {
        Some(true)
    } else if !carried_present {
        Some(false)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A disabled slot always re-enables, regardless of what is carried —
    /// vanilla's `if (this.menu.isSlotDisabled(slotId)) { this.enableSlot(...) }`
    /// has no carried-item condition on this arm.
    #[test]
    fn a_disabled_slot_always_re_enables() {
        assert_eq!(toggle_decision(true, false), Some(true));
        assert_eq!(toggle_decision(true, true), Some(true));
    }

    /// An enabled, empty slot disables only when nothing is carried —
    /// carrying an item must fall through to a normal placement instead
    /// (`else if (this.menu.getCarried().isEmpty())`).
    #[test]
    fn an_enabled_slot_disables_only_with_an_empty_cursor() {
        assert_eq!(toggle_decision(false, false), Some(false));
        assert_eq!(toggle_decision(false, true), None);
    }
}
