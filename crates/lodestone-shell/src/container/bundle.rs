//! Bundle scroll-to-select tracking — `BUNDLE_ITEM_SELECTED` /
//! #613's `SelectBundleItem` remainder, the client (producer) half.
//!
//! ## What it is
//!
//! Vanilla's `BundleMouseActions` scroll-selects which item inside a hovered
//! bundle a tooltip highlights, mutating the hovered `ItemStack`'s own
//! `BUNDLE_CONTENTS` component in place for the highlight and informing the
//! server with `ServerboundSelectBundleItemPacket` purely so a later
//! right-click (`removeOne()`) takes the highlighted item rather than the
//! front one — the component's own `selectedItem` never reaches the wire
//! (see [`lodestone_model::ItemComponents::bundle_contents`]'s doc), so this
//! is a one-way "FYI" send with no reply to predict against.
//!
//! ## How it works
//!
//! [`next_scroll_wheel_selection`] is `ScrollWheelHandler
//! .getNextScrollWheelSelection` transcribed: one wheel notch steps the
//! selection by exactly one slot, wrapping through `[0, limit)` — only the
//! notch's *sign* matters, not its magnitude, the same shape every other
//! scroll-driven single-step gesture in vanilla takes (compare this crate's
//! own `hotbar_scroll_step`).
//!
//! This shell's menu state is server-synced (`Menu`/`OpenMenuSnapshot`)
//! rather than a locally mutable `ItemStack`, so [`BundleSelection`] tracks
//! the highlight *beside* the menu instead of inside it: which slot is
//! selected, and at what index. [`bundle_slot_scrolled`] is the entry point —
//! it resolves one notch into the new tracked selection, or `None` when the
//! hovered slot holds no scrollable bundle (an empty bundle or a non-bundle
//! item), matching `onMouseScrolled`'s own `amountOfShownItems == 0` no-op
//! guard. The caller is responsible for clearing the tracked selection when
//! the slot stops being hovered or the container closes — this module has no
//! per-frame hook of its own to do that from.
//!
//! ## How to change it
//!
//! [`lodestone_game::item::is_bundle`] stands in for vanilla's
//! `#minecraft:bundles` tag — see its own doc for the gap and how to close it
//! properly if a modded item ever breaks the assumption it makes.
//!
//! ## Dependencies
//!
//! [`lodestone_game::item`] (`ItemStack::bundle_items_to_show`, `is_bundle`).

use lodestone_game::item::ItemStack;

/// `ScrollWheelHandler.getNextScrollWheelSelection`:
/// one notch steps by exactly one slot in the notch's direction, wrapping
/// through `[0, limit)`. Only `wheel`'s *sign* matters, matching the real
/// method's `Math.signum(wheel)`; `limit <= 0` (an empty or non-bundle slot)
/// is the caller's job to gate on before reaching here — see
/// [`bundle_slot_scrolled`].
#[must_use]
pub fn next_scroll_wheel_selection(wheel: f64, current: i32, limit: i32) -> i32 {
    if limit <= 0 {
        return -1;
    }
    let step = if wheel > 0.0 {
        1
    } else if wheel < 0.0 {
        -1
    } else {
        0
    };
    let mut selected = (current - step).max(-1);
    while selected < 0 {
        selected += limit;
    }
    while selected >= limit {
        selected -= limit;
    }
    selected
}

/// Which slot's bundle-highlight scroll this shell is currently tracking, and
/// at what index — the local stand-in for vanilla's in-place component
/// mutation (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BundleSelection {
    pub window_id: i32,
    pub slot: i32,
    pub selected: i32,
}

/// Resolves one scroll-wheel notch over `slot` (holding `stack`) into the new
/// tracked selection, or `None` when the slot holds nothing scrollable —
/// vanilla's own two-step gate, `BundleMouseActions.matches` (`is_bundle`)
/// then `onMouseScrolled`'s own `amountOfShownItems == 0` check
/// (`bundle_items_to_show`), kept as two checks here for the same reason:
/// `matches` runs before *any* wheel notch is looked at (it decides which
/// `ItemSlotMouseAction` even gets asked), where the shown-items count can
/// only be computed for a stack already known to carry the component.
///
/// `previous` is the selection already being tracked, if any, so scrolling
/// the *same* slot continues from its current index rather than restarting
/// at "nothing selected" every notch; scrolling a *different* slot (or none
/// tracked yet) starts fresh from `-1`, matching a freshly-hovered stack's
/// `getSelectedItemIndex()`.
#[must_use]
pub fn bundle_slot_scrolled(
    window_id: i32,
    slot: i32,
    stack: &ItemStack,
    wheel: f64,
    previous: Option<BundleSelection>,
) -> Option<BundleSelection> {
    if !lodestone_game::item::is_bundle(stack.item()) {
        return None;
    }
    let limit = stack.bundle_items_to_show();
    if limit == 0 {
        return None;
    }
    let current = previous
        .filter(|p| p.window_id == window_id && p.slot == slot)
        .map_or(-1, |p| p.selected);
    let selected = next_scroll_wheel_selection(wheel, current, limit as i32);
    Some(BundleSelection {
        window_id,
        slot,
        selected,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_model::Identifier;

    fn id(s: &str) -> Identifier {
        s.parse().expect("valid id")
    }

    /// `limit <= 0` is a defensive guard, never reachable through
    /// [`bundle_slot_scrolled`] (which gates on it first), but it must not
    /// wrap into a bogus positive index if ever called directly.
    #[test]
    fn a_non_positive_limit_reports_no_selection() {
        assert_eq!(next_scroll_wheel_selection(1.0, 0, 0), -1);
        assert_eq!(next_scroll_wheel_selection(-1.0, 2, -1), -1);
    }

    /// A zero notch (no whole scroll yet accumulated) is a no-op that still
    /// lands in range rather than escaping the wrap loop — `Math.signum(0.0)
    /// == 0`, so `getNextScrollWheelSelection`'s subtraction is a no-op too.
    #[test]
    fn a_zero_notch_leaves_the_selection_unchanged_once_in_range() {
        assert_eq!(next_scroll_wheel_selection(0.0, 1, 3), 1);
    }

    /// From "nothing selected" (`-1`), one notch either direction must still
    /// land inside `[0, limit)` — the `Math.max(-1, …)` clamp followed by the
    /// wrap-up loop, worked by hand against the real method.
    #[test]
    fn scrolling_from_unselected_wraps_into_range() {
        assert_eq!(
            next_scroll_wheel_selection(1.0, -1, 3),
            2,
            "a positive notch from unselected lands on the last slot"
        );
        assert_eq!(
            next_scroll_wheel_selection(-1.0, -1, 3),
            0,
            "a negative notch from unselected lands on the first slot"
        );
    }

    /// Repeated notches in one direction cycle through every slot and wrap
    /// back around, rather than saturating at an end.
    #[test]
    fn repeated_notches_cycle_and_wrap() {
        let limit = 3;
        let mut selected = -1;
        let mut seen = Vec::new();
        for _ in 0..limit {
            selected = next_scroll_wheel_selection(1.0, selected, limit);
            seen.push(selected);
        }
        assert_eq!(seen, vec![2, 1, 0], "one full cycle visits every slot exactly once");
        // The notch after a full cycle wraps back to where it started.
        assert_eq!(next_scroll_wheel_selection(1.0, selected, limit), 2);
    }

    fn bundle_of(count: usize) -> ItemStack {
        let mut stack = ItemStack::new(id("minecraft:bundle"), 1);
        stack.set_bundle_contents(
            (0..count)
                .map(|_| ItemStack::new(id("minecraft:torch"), 1))
                .collect(),
        );
        stack
    }

    /// An empty bundle and a non-bundle item both report no scrollable
    /// selection — `bundle_items_to_show() == 0` covers both without a
    /// separate `is_bundle` check.
    #[test]
    fn an_empty_bundle_and_a_plain_item_have_nothing_to_scroll() {
        let empty = ItemStack::new(id("minecraft:bundle"), 1);
        assert_eq!(bundle_slot_scrolled(1, 9, &empty, 1.0, None), None);

        let torch = ItemStack::new(id("minecraft:torch"), 1);
        assert_eq!(bundle_slot_scrolled(1, 9, &torch, 1.0, None), None);
    }

    /// Scrolling the same tracked slot again continues from its current
    /// index instead of restarting from unselected every notch.
    #[test]
    fn scrolling_the_same_slot_continues_the_running_selection() {
        let stack = bundle_of(3);
        let first = bundle_slot_scrolled(4, 9, &stack, 1.0, None).expect("a selection");
        assert_eq!(first, BundleSelection { window_id: 4, slot: 9, selected: 2 });

        let second =
            bundle_slot_scrolled(4, 9, &stack, 1.0, Some(first)).expect("still a selection");
        assert_eq!(second.selected, 1, "continues from 2, not restart at -1");
    }

    /// Scrolling a *different* slot starts a fresh selection rather than
    /// continuing the previous slot's index — the previous tracked selection
    /// must not leak across slots.
    #[test]
    fn scrolling_a_different_slot_starts_fresh() {
        let stack = bundle_of(3);
        let tracked = BundleSelection { window_id: 4, slot: 9, selected: 1 };
        let fresh =
            bundle_slot_scrolled(4, 20, &stack, 1.0, Some(tracked)).expect("a selection");
        assert_eq!(
            fresh,
            BundleSelection { window_id: 4, slot: 20, selected: 2 },
            "must start from unselected (-1), the same as no previous tracking at all"
        );
    }
}
