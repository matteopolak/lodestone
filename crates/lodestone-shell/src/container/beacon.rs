//! The beacon screen's power-selection buttons and confirm/cancel click
//! surface (`SetBeaconEffects` remainder).
//!
//! ## What it is
//!
//! `ClientAction::SetBeaconEffects` was encoded by every protocol family
//! (`crates/protocol/v770/src/adapter/serverbound.rs`'s `encode_set_beacon`)
//! with zero shell callers — the outbound-island shape `ClientAction::SetFlying`
//! was caught in. This module is the producer: [`power_buttons`]/
//! [`upgrade_button`] lay out `BeaconScreen`'s eight power buttons exactly
//! where vanilla puts them, [`BeaconSelection`] tracks which of them the
//! player has picked (mirroring `BeaconScreen`'s own screen-local
//! `primary`/`secondary` fields), and [`button_hit_test`] resolves a click
//! against both.
//!
//! ## How it works
//!
//! `BeaconMenu`'s three `container_data` properties are `0` = pyramid
//! `levels`, `1`/`2` = the primary/secondary power, each encoded
//! `BeaconMenu.encodeEffect`'s way: `0` for none, else the
//! `minecraft:mob_effect` registry id `+ 1`. [`BeaconSelection::sync`] is
//! `BeaconScreen`'s `ContainerListener::dataChanged` — it re-derives the
//! local selection from those two properties, but only when they actually
//! change, so a pending local click (not yet confirmed, and therefore not
//! yet reflected in `container_data`) survives frame to frame instead of
//! being stomped by a redundant resync. `AnvilRenameState::sync` is the
//! same shape for the identical reason: see its own doc.
//!
//! [`power_buttons`]' pixel arithmetic is `BeaconScreen.init`'s own
//! `leftPos + 76 + c*24 - totalWidth/2`/`topPos + 22 + tier*25` (tiers 0..=2)
//! and `leftPos + 167 + c*24 - totalWidth/2`/`topPos + 47` (tier 3),
//! `leftPos`/`topPos` folded to `0` since these are local widget pixels —
//! the same convention [`super::merchant`]'s `row_layout`/`button_rect`
//! already use.
//!
//! ## How to change it
//!
//! [`BEACON_EFFECT_TIERS`] is `BeaconBlockEntity.BEACON_EFFECTS`
//! (`.cache/mc/26.2/src`), duplicated rather than imported from
//! `lodestone-server`'s own copy (`crate::beacon::BEACON_EFFECT_TIERS`) —
//! that crate is off limits to this pass, and this repo already duplicates
//! small vanilla censuses per-module rather than share them across a crate
//! boundary it cannot reach (e.g. `fire.rs`/`growth_tick.rs`'s own private
//! `base_name` copies). If the two ever disagree, the server's copy is
//! authoritative; re-derive this one from vanilla's own beacon block entity again.
//!
//! `container::geometry`'s draw side blits the real vanilla art for these:
//! a `22x22` `container/beacon/button*` state sprite per button, then the
//! effect's own `mob_effect/<id>` icon `18x18` two pixels in — the two blits
//! `BeaconScreenButton.extractContents`/`extractIcon` make.
//!
//! It used to draw a hash-derived tint swatch instead, above a note saying no
//! effect-icon art existed in this tree. That was written from a search of
//! `gui/sprites/**`, which is genuinely where the button states live and
//! genuinely *not* where the effect icons do: `atlases/gui.json` gives the GUI
//! atlas a second source directory, `textures/mob_effect/**`, under a
//! `mob_effect/` prefix. `ContainerBackground` enumerates both. The swatch
//! survives only as the jar-less fallback.
//!
//! ## Dependencies
//!
//! [`lodestone_data::mob_effects`] (registry id ↔ `minecraft:*` name, for
//! decoding `container_data`'s encoded effect ids back to a
//! [`ResourceKey`]); [`super::layout`] (panel origin/scale, the same seam
//! every other click surface in this crate resolves a cursor through).

use lodestone_game::menu::{Menu, SpecialLayout};
use lodestone_model::ResourceKey;

use super::layout::Rect;

/// `BeaconBlockEntity.BEACON_EFFECTS` — the four beacon power tiers, index 0
/// = the tier a level-1 pyramid unlocks. Tier 3 (regeneration) is the
/// level-4-only, secondary-only power. See the module doc for why this is a
/// duplicate of `lodestone-server`'s own copy rather than a shared import.
pub const BEACON_EFFECT_TIERS: [&[&str]; 4] = [
    &["minecraft:speed", "minecraft:haste"],
    &["minecraft:resistance", "minecraft:jump_boost"],
    &["minecraft:strength"],
    &["minecraft:regeneration"],
];

/// One power button's identity and local-widget-pixel top-left corner.
#[derive(Debug, Clone, PartialEq)]
pub struct PowerButton {
    /// Pyramid tier that unlocks this button (`updateStatus`'s own `active =
    /// tier < levels`).
    pub tier: u8,
    /// Whether pressing this button sets the *primary* power (tiers 0..=2)
    /// or the *secondary* power (tier 3's regeneration slot and the dynamic
    /// upgrade slot).
    pub is_primary: bool,
    /// The effect this button picks.
    pub effect: ResourceKey,
    /// Local widget-pixel x of the button's top-left corner.
    pub x: f32,
    /// Local widget-pixel y of the button's top-left corner.
    pub y: f32,
}

/// A power button's square side length (`BeaconScreenButton`'s own `22, 22`).
pub const BUTTON: f32 = 22.0;

#[allow(clippy::cast_precision_loss)] // tier/count/index are always tiny (<= 4)
fn tier_row(tier: u8, effects: &[&str], base_x: f32, y: f32) -> Vec<PowerButton> {
    let count = effects.len();
    let total_width = count as f32 * 22.0 + (count.saturating_sub(1)) as f32 * 2.0;
    effects
        .iter()
        .enumerate()
        .map(|(c, name)| {
            let x = base_x + c as f32 * 24.0 - total_width / 2.0;
            PowerButton {
                tier,
                is_primary: tier < 3,
                effect: name
                    .parse()
                    .expect("BEACON_EFFECT_TIERS entries are valid minecraft: identifiers"),
                x,
                y,
            }
        })
        .collect()
}

/// `BeaconScreen.init`'s eight *static* power buttons: the three primary
/// rows (tiers 0..=2) and tier 3's single regeneration secondary. Does
/// **not** include the dynamic "upgrade current primary" secondary slot —
/// see [`upgrade_button`], which needs the live `primary` selection
/// `BeaconScreen.init` closes over instead.
#[must_use]
#[allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // tier/count/index are always tiny (<= 4)
pub fn power_buttons() -> Vec<PowerButton> {
    let mut buttons = Vec::new();
    for (tier, effects) in BEACON_EFFECT_TIERS.iter().take(3).enumerate() {
        let tier = tier as u8;
        buttons.extend(tier_row(tier, effects, 76.0, 22.0 + f32::from(tier) * 25.0));
    }
    // Tier 3's shared row: `count = BEACON_EFFECTS[3].len() + 1` (the
    // regeneration slot plus the dynamic upgrade slot) decides `totalWidth`,
    // but `init`'s own loop only places the first `count - 1` — the
    // regeneration slot — here; the last is `upgrade_button`.
    let tier3 = BEACON_EFFECT_TIERS[3];
    let count = tier3.len() + 1;
    let total_width = count as f32 * 22.0 + (count - 1) as f32 * 2.0;
    for (c, name) in tier3.iter().enumerate() {
        let x = 167.0 + c as f32 * 24.0 - total_width / 2.0;
        buttons.push(PowerButton {
            tier: 3,
            is_primary: false,
            effect: name
                .parse()
                .expect("BEACON_EFFECT_TIERS entries are valid minecraft: identifiers"),
            x,
            y: 47.0,
        });
    }
    buttons
}

/// `BeaconUpgradePowerButton` — the dynamic secondary slot that mirrors
/// whatever `primary` is currently chosen (`updateStatus`'s own `visible =
/// primary != null`; `setEffect(primary)` each time it changes). `None` with
/// no primary chosen yet, matching that visibility gate exactly: an
/// invisible vanilla button also cannot be pressed.
#[must_use]
#[allow(clippy::cast_precision_loss)] // count is always tiny (<= 4)
pub fn upgrade_button(primary: Option<&ResourceKey>) -> Option<PowerButton> {
    let effect = primary?.clone();
    let tier3 = BEACON_EFFECT_TIERS[3];
    let count = tier3.len() + 1;
    let total_width = count as f32 * 22.0 + (count - 1) as f32 * 2.0;
    let x = 167.0 + (count - 1) as f32 * 24.0 - total_width / 2.0;
    Some(PowerButton {
        tier: 3,
        is_primary: false,
        effect,
        x,
        y: 47.0,
    })
}

/// The confirm button (`BeaconScreen.CONFIRM_SPRITE`), local widget pixels.
#[must_use]
pub fn confirm_rect() -> Rect {
    Rect {
        x: 164.0,
        y: 107.0,
        w: BUTTON,
        h: BUTTON,
    }
}

/// The cancel button (`BeaconScreen.CANCEL_SPRITE`), local widget pixels.
#[must_use]
pub fn cancel_rect() -> Rect {
    Rect {
        x: 190.0,
        y: 107.0,
        w: BUTTON,
        h: BUTTON,
    }
}

fn hit(x: f32, y: f32, r: Rect) -> bool {
    x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
}

/// What a click at a resolved local-widget-pixel point hits, if anything.
#[derive(Debug, Clone, PartialEq)]
pub enum BeaconHit {
    /// A power button — `is_primary` says which local selection field to
    /// update.
    Power {
        /// See [`PowerButton::is_primary`].
        is_primary: bool,
        /// The effect this button picks.
        effect: ResourceKey,
    },
    /// The confirm button.
    Confirm,
    /// The cancel button.
    Cancel,
}

/// Resolves a **local widget-pixel** point to whatever it hits. `levels` and
/// `primary` gate exactly like vanilla's own `updateStatus`: a button whose
/// tier the pyramid has not unlocked is `active = false`, and vanilla's
/// button widgets never deliver a press to an inactive one, so this does not
/// hit-test them either — a click that lands on a disabled swatch falls
/// through as if nothing were there, matching what the player sees.
#[must_use]
pub fn hit_test_local(levels: i32, primary: Option<&ResourceKey>, x: f32, y: f32) -> Option<BeaconHit> {
    if hit(x, y, confirm_rect()) {
        return Some(BeaconHit::Confirm);
    }
    if hit(x, y, cancel_rect()) {
        return Some(BeaconHit::Cancel);
    }
    for button in power_buttons().into_iter().chain(upgrade_button(primary)) {
        if i32::from(button.tier) >= levels {
            continue;
        }
        if hit(
            x,
            y,
            Rect {
                x: button.x,
                y: button.y,
                w: BUTTON,
                h: BUTTON,
            },
        ) {
            return Some(BeaconHit::Power {
                is_primary: button.is_primary,
                effect: button.effect,
            });
        }
    }
    None
}

/// [`hit_test_local`] plus the panel-origin/scale resolution every other
/// click surface in this crate does — the same shape as
/// [`super::merchant::button_hit_test`]. `None` off any non-beacon screen.
#[must_use]
pub fn button_hit_test(
    menu: &Menu,
    gui_scale: u32,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
    levels: i32,
    primary: Option<&ResourceKey>,
) -> Option<BeaconHit> {
    if menu.special_layout() != Some(SpecialLayout::Beacon) {
        return None;
    }
    let layout = super::layout::slot_layout(menu);
    let (px, py) = super::layout::panel_origin_with_scale(&layout, gui_scale, width, height);
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    hit_test_local(levels, primary, x / scale - px, y / scale - py)
}

/// Local pending primary/secondary power selection — vanilla's
/// `BeaconScreen`'s own `primary`/`secondary` fields. See the module doc for
/// [`Self::sync`]'s relationship to `ContainerListener::dataChanged`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BeaconSelection {
    /// The currently chosen primary power, or `None`.
    pub primary: Option<ResourceKey>,
    /// The currently chosen secondary power, or `None`.
    pub secondary: Option<ResourceKey>,
    /// The last `(primary_id, secondary_id)` pair [`Self::sync`] resolved
    /// against — `container_data` properties `1`/`2`, `BeaconMenu.encodeEffect`'s
    /// wire form. `None` before the first sync (nothing observed yet).
    signature: Option<(i32, i32)>,
}

fn decode_effect(id: i32) -> Option<ResourceKey> {
    if id == 0 {
        return None;
    }
    let id = lodestone_data::mob_effects::MobEffectId::from_registry_id(id - 1)?;
    lodestone_data::mob_effects::mob_effect_name_for(id).parse().ok()
}

impl BeaconSelection {
    /// A fresh selection with nothing chosen and nothing observed yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// `ContainerListener::dataChanged`'s reset: re-derives
    /// [`Self::primary`]/[`Self::secondary`] from `container_data`
    /// properties `1`/`2` exactly when that pair changes (vanilla fires this
    /// on *every* `beaconData.set` — the initial full send on open, and
    /// every successful `SET_BEACON` confirm), and leaves a pending local
    /// click alone otherwise. Returns whether a reset happened.
    pub fn sync(&mut self, primary_id: i32, secondary_id: i32) -> bool {
        let signature = Some((primary_id, secondary_id));
        if signature == self.signature {
            return false;
        }
        self.primary = decode_effect(primary_id);
        self.secondary = decode_effect(secondary_id);
        self.signature = signature;
        true
    }

    /// `BeaconPowerButton.onPress` for a **primary** tier button: a no-op if
    /// it is already selected (`isSelected()`'s guard); otherwise it becomes
    /// the primary, and the secondary is cleared unless it is the *exact
    /// same* effect (the level-II amplifier boost, `Objects.equals`).
    pub fn select_primary(&mut self, effect: ResourceKey) {
        if self.primary.as_ref() == Some(&effect) {
            return;
        }
        if self.secondary.as_ref() != Some(&effect) {
            self.secondary = None;
        }
        self.primary = Some(effect);
    }

    /// `BeaconPowerButton.onPress` for a **secondary** button (the tier-3
    /// regeneration slot or the dynamic upgrade slot): the same no-op guard,
    /// no clearing.
    pub fn select_secondary(&mut self, effect: ResourceKey) {
        if self.secondary.as_ref() == Some(&effect) {
            return;
        }
        self.secondary = Some(effect);
    }

    /// `BeaconConfirmButton.updateStatus`'s `active` gate: a payment item
    /// must occupy the payment slot and a primary power must be chosen.
    #[must_use]
    pub fn can_confirm(&self, has_payment: bool) -> bool {
        has_payment && self.primary.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn power_buttons_match_vanillas_transcribed_arithmetic() {
        let buttons = power_buttons();
        // Tier 0 (speed, haste): count=2, totalWidth=2*22+2=46.
        // c=0: x = 76 + 0 - 23 = 53, y = 22.
        // c=1: x = 76 + 24 - 23 = 77, y = 22.
        assert_eq!(buttons[0].x, 53.0);
        assert_eq!(buttons[0].y, 22.0);
        assert_eq!(buttons[0].effect, "minecraft:speed".parse().unwrap());
        assert!(buttons[0].is_primary);
        assert_eq!(buttons[1].x, 77.0);
        assert_eq!(buttons[1].y, 22.0);
        // Tier 2 (strength): count=1, totalWidth=22. c=0: x=76-11=65, y=22+2*25=72.
        let strength = buttons.iter().find(|b| b.tier == 2).unwrap();
        assert_eq!(strength.x, 65.0);
        assert_eq!(strength.y, 72.0);
        assert!(strength.is_primary);
        // Tier 3 (regeneration): count = 1+1 = 2, totalWidth = 2*22+2 = 46.
        // c=0: x = 167 + 0 - 23 = 144, y = 47.
        let regen = buttons.iter().find(|b| b.tier == 3).unwrap();
        assert_eq!(regen.x, 144.0);
        assert_eq!(regen.y, 47.0);
        assert!(!regen.is_primary);
        assert_eq!(buttons.len(), 2 + 2 + 1 + 1);
    }

    #[test]
    fn upgrade_button_is_none_with_no_primary_and_tracks_it_otherwise() {
        assert_eq!(upgrade_button(None), None);
        let strength: ResourceKey = "minecraft:strength".parse().unwrap();
        let button = upgrade_button(Some(&strength)).unwrap();
        assert_eq!(button.effect, strength);
        assert_eq!(button.tier, 3);
        assert!(!button.is_primary);
        // count=2, totalWidth=46, c=count-1=1: x = 167 + 24 - 23 = 168.
        assert_eq!(button.x, 168.0);
        assert_eq!(button.y, 47.0);
    }

    #[test]
    fn hit_test_ignores_a_tier_the_pyramid_has_not_unlocked() {
        let button = &power_buttons()[0]; // tier 0, x=53, y=22
        let cx = button.x + 1.0;
        let cy = button.y + 1.0;
        assert_eq!(hit_test_local(0, None, cx, cy), None, "tier 0 needs levels >= 1");
        assert_eq!(
            hit_test_local(1, None, cx, cy),
            Some(BeaconHit::Power {
                is_primary: true,
                effect: "minecraft:speed".parse().unwrap()
            })
        );
    }

    #[test]
    fn hit_test_finds_confirm_and_cancel_regardless_of_levels() {
        let confirm = confirm_rect();
        assert_eq!(
            hit_test_local(0, None, confirm.x + 1.0, confirm.y + 1.0),
            Some(BeaconHit::Confirm)
        );
        let cancel = cancel_rect();
        assert_eq!(
            hit_test_local(0, None, cancel.x + 1.0, cancel.y + 1.0),
            Some(BeaconHit::Cancel)
        );
    }

    #[test]
    fn selection_sync_reseeds_from_container_data_exactly_once_per_change() {
        let mut sel = BeaconSelection::new();
        // id 0 = none. speed is index 0 -> encoded id 1.
        assert!(sel.sync(1, 0));
        assert_eq!(sel.primary, Some("minecraft:speed".parse().unwrap()));
        assert_eq!(sel.secondary, None);
        // Same pair again: no reset, so a pending local pick below survives.
        assert!(!sel.sync(1, 0));
    }

    #[test]
    fn selecting_a_new_primary_clears_a_different_secondary_but_keeps_a_matching_one() {
        let mut sel = BeaconSelection::new();
        let speed: ResourceKey = "minecraft:speed".parse().unwrap();
        let strength: ResourceKey = "minecraft:strength".parse().unwrap();
        let regen: ResourceKey = "minecraft:regeneration".parse().unwrap();

        sel.select_primary(speed.clone());
        sel.select_secondary(regen.clone());
        assert_eq!(sel.secondary, Some(regen));

        // A different primary clears the mismatched secondary.
        sel.select_primary(strength.clone());
        assert_eq!(sel.primary, Some(strength.clone()));
        assert_eq!(sel.secondary, None, "a different primary clears the secondary");

        // The upgrade case: secondary == primary survives re-selecting the
        // same primary (the `isSelected()` no-op guard means this needs a
        // *different* primary in between to exercise the non-clearing arm).
        sel.select_primary(speed.clone());
        sel.select_secondary(strength.clone());
        assert_eq!(sel.secondary, Some(strength.clone()));
        sel.select_primary(strength);
        assert_eq!(
            sel.secondary,
            Some("minecraft:strength".parse().unwrap()),
            "picking the same effect as both primary and secondary is the amplifier boost, not a clear"
        );
    }

    #[test]
    fn confirm_needs_both_payment_and_a_chosen_primary() {
        let mut sel = BeaconSelection::new();
        assert!(!sel.can_confirm(true), "no primary chosen yet");
        sel.select_primary("minecraft:speed".parse().unwrap());
        assert!(!sel.can_confirm(false), "no payment in the slot");
        assert!(sel.can_confirm(true));
    }
}
