//! The local player's first-person hand state: which stack each hand is
//! *showing* (which lags the stack it is *holding*) and how far each hand is
//! raised, advanced once per fixed game tick.
//!
//! # Why this lives in `Sim` and not in the renderer
//!
//! The reference client keeps this pair on the local player and advances it in
//! the player's own tick, straight after the attack counters advance. Two of its
//! inputs are tick-rate player facts the renderer cannot see — the
//! attack-cooldown scale and the boat-paddling "hands busy" bit — and one input
//! is an event (a successful item use snaps the used hand down), so a
//! wall-clock copy in the renderer could only ever approximate the phase. The
//! renderer now receives a finished per-frame sample
//! ([`FirstPersonHandsSample`]) and draws it.
//!
//! # The per-tick rule, per hand
//!
//! 1. Save last tick's height (the partial-tick lerp's start).
//! 2. If the shown stack still *matches* the held one (same item, same count,
//!    every data component equal except damage), adopt the held stack at once.
//! 3. Unless hands are busy, step the height toward a target by at most
//!    [`HAND_HEIGHT_STEP`]: `0` while the shown stack differs from the held one,
//!    otherwise `1` for the off hand and the cubed item-swap scale for the main
//!    hand. While hands are busy both heights instead fall by the step, floored
//!    at `0`, whatever is held.
//! 4. Once a height is below [`HAND_EXCHANGE_BELOW`], exchange the shown stack
//!    for the held one — the swap happens out of sight, at the bottom of the dip.

use lodestone_game::item::{DAMAGE_COMPONENT, ItemStack};

/// The most a hand's height moves in one tick, in either direction.
///
/// A full lower-then-raise is therefore `2 · 1/0.4` ticks plus the exchange
/// tick: `0.6, 0.2, 0.0` down and `0.4, 0.8, 1.0` up.
pub(crate) const HAND_HEIGHT_STEP: f32 = 0.4;

/// Below this height the shown stack is exchanged for the held one.
pub(crate) const HAND_EXCHANGE_BELOW: f32 = 0.1;

/// Whether `shown` can be replaced by `held` without a lowering: equal item,
/// equal count, and equal data components once damage is set aside. Damage is
/// the one component excluded, so a tool losing durability while mining does
/// not dip the hand, but eating one of a stack of bread does.
#[must_use]
pub(crate) fn stacks_match_for_swap(shown: Option<&ItemStack>, held: Option<&ItemStack>) -> bool {
    match (shown, held) {
        (None, None) => true,
        (Some(a), Some(b)) => {
            if a.count() != b.count() || a.item() != b.item() {
                return false;
            }
            let damage: lodestone_model::Identifier =
                DAMAGE_COMPONENT.parse().expect("valid built-in identifier");
            let ca: Vec<_> = a.components().iter().filter(|(k, _)| **k != damage).collect();
            let cb: Vec<_> = b.components().iter().filter(|(k, _)| **k != damage).collect();
            ca == cb
        }
        _ => false,
    }
}

/// One hand's shown stack and its raise height.
#[derive(Debug, Clone, Default, PartialEq)]
struct HandHeight {
    shown: Option<ItemStack>,
    /// `0.0` fully lowered, `1.0` fully raised.
    height: f32,
    /// Last tick's [`Self::height`].
    previous: f32,
}

impl HandHeight {
    /// `1 - lerp(partial, previous, height)`: how far *below* rest the hand
    /// is drawn, the value the hand pose multiplies by `-0.6` blocks.
    fn inverse_arm_height(&self, partial: f32) -> f32 {
        let partial = partial.clamp(0.0, 1.0);
        1.0 - (self.previous + (self.height - self.previous) * partial)
    }

    fn step_toward(&mut self, target: f32) {
        self.height += (target - self.height).clamp(-HAND_HEIGHT_STEP, HAND_HEIGHT_STEP);
    }
}

/// This tick's inputs to [`FirstPersonHands::tick`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct HandTickInput<'a> {
    /// The stack actually in the selected hotbar slot.
    pub main: Option<&'a ItemStack>,
    /// The stack actually in the off-hand slot.
    pub off: Option<&'a ItemStack>,
    /// Paddling a boat: both hands are on the oars.
    pub hands_busy: bool,
    /// `clamp((item_swap_ticks + 1) / attack_delay, 0, 1)`, the main hand's
    /// cooldown scale; `1.0` is a fully recovered hand.
    pub main_swap_scale: f32,
}

/// Both first-person hands. Starts empty and fully lowered, as the reference
/// client's fresh player does: the first ticks in a world raise the hand into
/// view.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FirstPersonHands {
    main: HandHeight,
    off: HandHeight,
}

impl FirstPersonHands {
    /// Advance both hands by one fixed tick. See the module doc for the rule.
    pub(crate) fn tick(&mut self, input: HandTickInput<'_>) {
        self.main.previous = self.main.height;
        self.off.previous = self.off.height;

        let main_matches = stacks_match_for_swap(self.main.shown.as_ref(), input.main);
        if main_matches {
            self.main.shown = input.main.cloned();
        }
        let off_matches = stacks_match_for_swap(self.off.shown.as_ref(), input.off);
        if off_matches {
            self.off.shown = input.off.cloned();
        }

        if input.hands_busy {
            self.main.height = (self.main.height - HAND_HEIGHT_STEP).clamp(0.0, 1.0);
            self.off.height = (self.off.height - HAND_HEIGHT_STEP).clamp(0.0, 1.0);
        } else {
            let scale = input.main_swap_scale.clamp(0.0, 1.0);
            let main_target = if main_matches { scale * scale * scale } else { 0.0 };
            let off_target = if off_matches { 1.0 } else { 0.0 };
            self.main.step_toward(main_target);
            self.off.step_toward(off_target);
        }

        if self.main.height < HAND_EXCHANGE_BELOW {
            self.main.shown = input.main.cloned();
        }
        if self.off.height < HAND_EXCHANGE_BELOW {
            self.off.shown = input.off.cloned();
        }
    }

    /// Everything the hand pass needs this frame, at partial tick `partial`.
    #[must_use]
    pub(crate) fn sample(&self, partial: f32) -> (HandSample, HandSample) {
        (
            HandSample {
                shown: self.main.shown.clone(),
                inverse_arm_height: self.main.inverse_arm_height(partial),
            },
            HandSample {
                shown: self.off.shown.clone(),
                inverse_arm_height: self.off.inverse_arm_height(partial),
            },
        )
    }
}

/// One hand of a [`FirstPersonHandsSample`].
#[derive(Debug, Clone, PartialEq)]
pub struct HandSample {
    /// The stack the hand is showing, which lags the held one across a swap.
    pub shown: Option<ItemStack>,
    /// How far below rest the hand is drawn, `0.0..=1.0`.
    pub inverse_arm_height: f32,
}

/// The local player's first-person hands for one frame, as `Sim` publishes
/// them to the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct FirstPersonHandsSample {
    /// The main hand.
    pub main: HandSample,
    /// The off hand.
    pub off: HandSample,
    /// Whether the main hand draws at all this frame.
    pub render_main: bool,
    /// Whether the off hand draws at all this frame.
    pub render_off: bool,
}

/// Which hands draw, from what the player actually holds and uses. A bow or
/// crossbow being drawn hides the other hand; everything else draws both.
///
/// Only the main hand is ever used by this client, so "the used hand" is the
/// main hand whenever `using` is set. A charged crossbow is meant to hide the
/// off hand too, but stack charge is not carried in the local inventory, so
/// every crossbow reads as uncharged here.
#[must_use]
pub(crate) fn hands_to_render(main: Option<&ItemStack>, using: bool) -> (bool, bool) {
    let is_ranged = |stack: Option<&ItemStack>| {
        stack.is_some_and(|s| {
            let id = s.item().to_string();
            id == "minecraft:bow" || id == "minecraft:crossbow"
        })
    };
    if using && is_ranged(main) {
        (true, false)
    } else {
        (true, true)
    }
}

/// Convert a [`FirstPersonHandsSample`] into the renderer's per-frame
/// [`crate::gpu::FirstPersonHandsFrame`], resolving each shown stack through
/// the same draw record the hotbar uses, so a stack's model, tint, patterns
/// and skin cannot differ between the slot and the hand.
///
/// Right-handed: this client has no main-arm option and reports a right main
/// hand to the server.
#[must_use]
pub(crate) fn hands_frame(sample: &FirstPersonHandsSample) -> crate::gpu::FirstPersonHandsFrame {
    let hand = |hand: &HandSample, drawn: bool| crate::gpu::HandFrame {
        item: hand
            .shown
            .as_ref()
            .and_then(crate::hud::item_icon::stack_icon)
            .map(|icon| crate::hud::item_icon::held_item_record(&icon)),
        inverse_arm_height: hand.inverse_arm_height,
        drawn,
    };
    crate::gpu::FirstPersonHandsFrame {
        main_arm: lodestone_render::entity::Arm::Right,
        main: hand(&sample.main, sample.render_main),
        off: hand(&sample.off, sample.render_off),
    }
}

impl super::Sim {
    /// The two hand stacks actually held this tick, read without cloning the
    /// whole menu.
    fn held_hand_stacks(&self) -> (Option<ItemStack>, Option<ItemStack>) {
        let slot = self.selected_slot();
        self.read(|w| {
            let Some(menus) = w.get::<lodestone_ecs::SessionMenus>(self.local) else {
                return (None, None);
            };
            let menu = menus.0.player();
            let pick = |index: usize| menu.player_native(index).filter(|s| !s.is_empty()).cloned();
            (pick(slot), pick(super::OFFHAND_NATIVE_INDEX))
        })
    }

    /// One fixed tick of the first-person hands. Called from [`Self::step`]'s
    /// tick loop after the `GameTick` schedule, i.e. after this tick's attack
    /// counters advanced, which is the order the cooldown scale is read in.
    pub(crate) fn tick_first_person_hands(&mut self) {
        let (main, off) = self.held_hand_stacks();
        let input = HandTickInput {
            main: main.as_ref(),
            off: off.as_ref(),
            hands_busy: false,
            main_swap_scale: 1.0,
        };
        self.first_person_hands.tick(input);
    }

    /// This frame's first-person hands, interpolated at the frame's own
    /// partial tick. Install it with `RenderState::set_first_person_hands`.
    #[must_use]
    pub fn first_person_hands_sample(&self) -> FirstPersonHandsSample {
        let (main, off) = self.first_person_hands.sample(self.clock().interp_alpha);
        let (held_main, _) = self.held_hand_stacks();
        let using = self.read(|w| w.resource::<crate::interact::UsingItem>().0);
        let (render_main, render_off) = hands_to_render(held_main.as_ref(), using);
        FirstPersonHandsSample {
            main,
            off,
            render_main,
            render_off,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(id: &str, count: i32) -> ItemStack {
        ItemStack::new(id.parse().expect("valid id"), count)
    }

    fn rest_input<'a>(main: Option<&'a ItemStack>, off: Option<&'a ItemStack>) -> HandTickInput<'a> {
        HandTickInput {
            main,
            off,
            hands_busy: false,
            main_swap_scale: 1.0,
        }
    }

    /// A hand that has been holding `stack` long enough to be fully raised.
    fn settled(main: Option<&ItemStack>, off: Option<&ItemStack>) -> FirstPersonHands {
        let mut hands = FirstPersonHands::default();
        for _ in 0..10 {
            hands.tick(rest_input(main, off));
        }
        hands
    }

    /// From the empty, lowered start the hands rise `0.4, 0.8, 1.0`: three
    /// clamped steps of `0.4` toward `1`, the last one short.
    #[test]
    fn a_fresh_player_raises_both_hands_into_view() {
        let mut hands = FirstPersonHands::default();
        let mut heights = Vec::new();
        for _ in 0..4 {
            hands.tick(rest_input(None, None));
            heights.push((hands.main.height, hands.off.height));
        }
        let want = [0.4, 0.8, 1.0, 1.0];
        for ((main, off), want) in heights.iter().zip(want) {
            assert!((main - want).abs() < 1e-6 && (off - want).abs() < 1e-6, "{heights:?}");
        }
    }

    /// The off hand swaps on its own clock: changing only the off-hand stack
    /// lowers the off hand `0.6, 0.2, 0.0`, exchanges at the bottom, raises
    /// `0.4, 0.8, 1.0`, and never moves the main hand.
    #[test]
    fn the_off_hand_has_its_own_swap() {
        let sword = stack("minecraft:iron_sword", 1);
        let shield = stack("minecraft:shield", 1);
        let totem = stack("minecraft:totem_of_undying", 1);
        let mut hands = settled(Some(&sword), Some(&shield));
        let mut off_heights = Vec::new();
        let mut exchanged_at = None;
        for tick in 0..7 {
            hands.tick(rest_input(Some(&sword), Some(&totem)));
            off_heights.push(hands.off.height);
            assert_eq!(hands.main.height, 1.0, "the main hand must not move");
            if exchanged_at.is_none() && hands.off.shown.as_ref() == Some(&totem) {
                exchanged_at = Some(tick);
            }
        }
        let want = [0.6, 0.2, 0.0, 0.4, 0.8, 1.0, 1.0];
        for (got, want) in off_heights.iter().zip(want) {
            assert!((got - want).abs() < 1e-6, "{off_heights:?}");
        }
        assert_eq!(exchanged_at, Some(2), "the exchange lands on the tick height < 0.1");
    }

    /// Damage alone is not a swap; count is.
    #[test]
    fn durability_loss_does_not_dip_but_a_count_change_does() {
        let mut pick = stack("minecraft:iron_pickaxe", 1);
        let mut hands = settled(Some(&pick), None);
        pick.components_mut().insert(
            DAMAGE_COMPONENT.parse().unwrap(),
            lodestone_game::item::ComponentValue::Int(3),
        );
        hands.tick(rest_input(Some(&pick), None));
        assert_eq!(hands.main.height, 1.0);
        assert_eq!(hands.main.shown.as_ref(), Some(&pick), "the damaged stack is adopted at once");

        let bread = stack("minecraft:bread", 5);
        let mut hands = settled(Some(&bread), None);
        let fewer = stack("minecraft:bread", 4);
        hands.tick(rest_input(Some(&fewer), None));
        assert!((hands.main.height - 0.6).abs() < 1e-6, "one bread eaten lowers the hand");
    }

    /// `1 - lerp(p, previous, height)`: a quarter tick into the first lowering
    /// step (`1.0 → 0.6`) the hand is `0.1` below rest.
    #[test]
    fn the_sample_interpolates_from_last_tick() {
        let a = stack("minecraft:stick", 1);
        let b = stack("minecraft:stone", 1);
        let mut hands = settled(Some(&a), None);
        hands.tick(rest_input(Some(&b), None));
        let (main, off) = hands.sample(0.25);
        assert!((main.inverse_arm_height - 0.1).abs() < 1e-6, "{}", main.inverse_arm_height);
        assert_eq!(off.inverse_arm_height, 0.0);
        let (main, _) = hands.sample(0.0);
        assert!(main.inverse_arm_height.abs() < 1e-6, "p = 0 is last tick's rest");
    }

    /// Drawing a bow hides the off hand; holding one does not.
    #[test]
    fn a_drawn_bow_hides_the_off_hand() {
        let bow = stack("minecraft:bow", 1);
        assert_eq!(hands_to_render(Some(&bow), true), (true, false));
        assert_eq!(hands_to_render(Some(&bow), false), (true, true));
        let bread = stack("minecraft:bread", 1);
        assert_eq!(hands_to_render(Some(&bread), true), (true, true));
    }
}
