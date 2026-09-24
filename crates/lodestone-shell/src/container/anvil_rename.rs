//! The anvil's rename box: a persistent per-menu editable-text value —
//! vanilla's own anvil screen's `name` `EditBox` and its `onNameChanged`/
//! `slotChanged` pair.
//!
//! ## What it is
//!
//! Before this module, `ContainerFrame::anvil_name` could only ever be
//! whichever hover name the input slot happened to have: there was no
//! per-keystroke state anywhere in the crate (see that field's own doc,
//! pre-fix). [`AnvilRenameState`] is that state — the value the player has
//! typed, and enough of `slot 0`'s last-seen identity to know when vanilla's
//! `slotChanged` would have reset it.
//!
//! ## Why a plain `String`, not a [`crate::menu::edit_box::EditBox`]
//!
//! Every other free-typed box that lives *inside* a gameplay screen rather
//! than a [`crate::menu::nav::MenuNav`] screen — the recipe book's search box,
//! the creative inventory's search box — is a plain `String` edited by
//! append/backspace, not a real [`EditBox`](crate::menu::edit_box::EditBox):
//! see `app::creative_screen::edit_creative_search` and `app/lifecycle.rs`'s
//! `KeyOutcome::RecipeSearch`/`CreativeSearch` arms. This follows that
//! established shape rather than introducing a second one — no cursor
//! position, no selection, no clipboard. If a real caret/selection is ever
//! wanted here, port from vanilla's own `EditBox`, not from this module.
//!
//! ## The two rules `onNameChanged` needs, both ported exactly
//!
//! ```text
//! private void onNameChanged(String name) {
//!    Slot slot = this.menu.getSlot(0);
//!    if (slot.hasItem()) {
//!       String newName = name;
//!       if (!slot.getItem().has(DataComponents.CUSTOM_NAME)
//!             && newName.equals(slot.getItem().getHoverName().getString())) {
//!          newName = "";
//!       }
//!       if (this.menu.setItemName(newName)) {
//!          connection.send(new ServerboundRenameItemPacket(newName));
//!       }
//!    }
//! }
//! ```
//!
//! 1. **Normalise to empty only when the item has no custom name of its own
//!    *and* the typed text equals its default hover name.** An item that
//!    *already* carries a custom name does **not** normalise even if the
//!    player types that exact name back — see
//!    [`resolve_rename`](AnvilRenameState::resolve_rename)'s test for the
//!    two cases side by side, which is the discriminating pair: a fixture
//!    that only exercises the no-custom-name case cannot see the other one
//!    getting dropped.
//! 2. **Nothing to send while the input slot is empty** — vanilla's
//!    `slot.hasItem()` guard. [`resolve_rename`](AnvilRenameState::resolve_rename)
//!    returns `None` for exactly that case, not `Some(String::new())`, so a
//!    caller cannot confuse "send an empty rename" with "there is nothing to
//!    rename".
//!
//! `menu.setItemName(newName)` — the client-side prediction gate that decides
//! *whether* a changed name is worth sending at all — is not modelled: this
//! shell has no local item-rename prediction to consult, so every value
//! change is sent. A duplicate send of the same name the server already has
//! is idempotent and costs one packet, which is the honest simplification
//! given there is no prediction state to ask instead.
//!
//! ## `slotChanged`'s reset, approximated
//!
//! Vanilla resets the box to the input slot's own hover name (and refocuses
//! it) whenever `AbstractContainerMenu`'s broadcast-changes machinery detects
//! slot 0's `ItemStack` differs from what it last broadcast — a mechanism
//! this shell does not reproduce. [`AnvilRenameState::sync`] approximates it
//! with a cheap **signature** comparison instead: `(has_custom_name,
//! hover_name)` for whatever is in slot 0 right now versus last frame. That
//! catches every case that matters here — an item placed into or removed from
//! the slot, or swapped for a different one — at the cost of not
//! re-triggering on a same-signature broadcast that changes nothing visible
//! (which would be a no-op reset anyway).
//!
//! ## Dependencies
//!
//! None beyond `std` — this module is pure data and logic, read by
//! `app`'s per-frame sync (slot 0's signature) and per-keystroke edit (a
//! typed character or Backspace), and by
//! [`super::ContainerFrame::with_anvil_name`] for the draw.

/// The anvil rename box's live value plus enough of the input slot's last
/// known identity to reproduce vanilla's `slotChanged` reset. See the module
/// doc.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AnvilRenameState {
    /// The box's current text — what the player has typed, or the input
    /// item's own hover name if nothing has been typed over it yet.
    pub value: String,
    /// `(has_custom_name, hover_name)` for whatever [`Self::sync`] last saw in
    /// slot 0, or `None` for an empty slot. `None` is also what makes
    /// [`Self::resolve_rename`] report "nothing to rename" — see that
    /// method's own doc.
    signature: Option<(bool, String)>,
}

impl AnvilRenameState {
    /// A fresh box with nothing typed and no slot observed yet — the state a
    /// freshly opened anvil screen starts in, before the first [`Self::sync`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Vanilla's `slotChanged` for slot 0: `has_custom_name`/`hover_name`
    /// describe the item currently in the anvil's input slot, or `None` for
    /// an empty slot. Resets [`Self::value`] to the item's own hover name
    /// (or clears it) exactly when the signature actually changed — see the
    /// module doc on why a signature stands in for vanilla's broadcast-diff
    /// trigger.
    ///
    /// Returns whether a reset happened, so a caller that also owns focus
    /// state can refocus the box the way vanilla's own `slotChanged` does
    /// (`this.setFocused(this.name)`).
    pub fn sync(&mut self, item: Option<(bool, &str)>) -> bool {
        let signature = item.map(|(has_custom_name, name)| (has_custom_name, name.to_owned()));
        if signature == self.signature {
            return false;
        }
        self.value = signature
            .as_ref()
            .map_or_else(String::new, |(_, name)| name.clone());
        self.signature = signature;
        true
    }

    /// One printable character, appended — vanilla's `EditBox.charTyped`
    /// minus the input filter, which is already the whole of
    /// `EditBox::insert_text`'s job for a widget with no selection to
    /// replace. `setMaxLength(50)` (`AnvilScreen.subInit`).
    pub fn push_char(&mut self, ch: char) {
        const MAX_LEN: usize = 50;
        if self.value.chars().count() < MAX_LEN {
            self.value.push(ch);
        }
    }

    /// Backspace: delete the last character.
    pub fn backspace(&mut self) {
        self.value.pop();
    }

    /// `onNameChanged`'s whole body, minus the send: `None` when slot 0 is
    /// empty (nothing to rename — vanilla's `slot.hasItem()` guard), `Some`
    /// otherwise, already normalised to `""` when the typed text is the
    /// item's own unmodified default name. See the module doc's two rules.
    #[must_use]
    pub fn resolve_rename(&self) -> Option<String> {
        let (has_custom_name, default_name) = self.signature.as_ref()?;
        if !has_custom_name && self.value == *default_name {
            Some(String::new())
        } else {
            Some(self.value.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_state_has_nothing_to_rename() {
        let state = AnvilRenameState::new();
        assert_eq!(state.resolve_rename(), None, "no slot observed yet");
    }

    #[test]
    fn sync_seeds_the_value_from_the_items_own_hover_name_and_reports_a_reset() {
        let mut state = AnvilRenameState::new();
        assert!(state.sync(Some((false, "Diamond Sword"))));
        assert_eq!(state.value, "Diamond Sword");
        // A second sync with the *same* signature is not a reset.
        assert!(!state.sync(Some((false, "Diamond Sword"))));
    }

    #[test]
    fn an_empty_slot_resets_to_empty_and_has_nothing_to_rename() {
        let mut state = AnvilRenameState::new();
        state.sync(Some((false, "Diamond Sword")));
        state.push_char('!');
        assert!(state.sync(None), "the slot going empty is itself a reset");
        assert_eq!(state.value, "");
        assert_eq!(state.resolve_rename(), None);
    }

    #[test]
    fn typing_the_default_name_back_normalises_to_empty_only_without_a_custom_name() {
        // The discriminating pair from the module doc: two items whose
        // current hover name is identical, differing only in whether that
        // name is a *custom* one. Typing that exact text back must behave
        // differently for each — a fixture using only one of them cannot see
        // the other's rule dropped.
        let mut no_custom_name = AnvilRenameState::new();
        no_custom_name.sync(Some((false, "Diamond Sword")));
        for ch in "Diamond Sword".chars() {
            no_custom_name.push_char(ch);
        }
        // Wait — `sync` already seeded the value with the full name; clear
        // and retype to simulate the player actually typing it, matching the
        // `onNameChanged` responder firing on every keystroke.
        no_custom_name.value.clear();
        for ch in "Diamond Sword".chars() {
            no_custom_name.push_char(ch);
        }
        assert_eq!(
            no_custom_name.resolve_rename(),
            Some(String::new()),
            "no CUSTOM_NAME component + text matches the default => normalises to empty"
        );

        let mut already_named = AnvilRenameState::new();
        already_named.sync(Some((true, "Diamond Sword")));
        already_named.value.clear();
        for ch in "Diamond Sword".chars() {
            already_named.push_char(ch);
        }
        assert_eq!(
            already_named.resolve_rename(),
            Some("Diamond Sword".to_string()),
            "already has a CUSTOM_NAME component => typing the same text back \
             must NOT normalise to empty, even though the strings match"
        );
    }

    #[test]
    fn a_genuinely_new_name_is_sent_verbatim() {
        let mut state = AnvilRenameState::new();
        state.sync(Some((false, "Diamond Sword")));
        state.value.clear();
        for ch in "Excalibur".chars() {
            state.push_char(ch);
        }
        assert_eq!(state.resolve_rename(), Some("Excalibur".to_string()));
    }

    #[test]
    fn backspace_and_the_max_length_cap_both_hold() {
        let mut state = AnvilRenameState::new();
        state.sync(Some((false, "")));
        for ch in "abc".chars() {
            state.push_char(ch);
        }
        assert_eq!(state.value, "abc");
        state.backspace();
        assert_eq!(state.value, "ab");

        let mut long = AnvilRenameState::new();
        long.sync(Some((false, "")));
        for _ in 0..60 {
            long.push_char('x');
        }
        assert_eq!(long.value.chars().count(), 50, "AnvilScreen.subInit's setMaxLength(50)");
    }
}
