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
//!
//! A successful item use ([`FirstPersonHands::item_used`]) sets the used hand's
//! height to `0` outright; the next ticks raise it again by rule 3. That snap is
//! the only height change an event makes — it never touches the arm swing.
//!
//! "Hands busy" is paddling: controlling a boat with any movement key held.
//! Besides lowering both hands it refuses attack and use clicks outright
//! ([`super::Sim::hands_busy`]); letting go raises the hands by rule 3.

use lodestone_client::Hand;
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
    /// The main-hand item id seen on the previous tick: a change to a
    /// *different item* restarts both attack counters (so switching weapons
    /// both empties the cooldown and lowers the new item along the cooldown
    /// curve). Count and component changes of the same item do not.
    last_main_item: Option<lodestone_model::Identifier>,
    /// The last tick's "hands busy" (paddling) bit.
    hands_busy: bool,
}

impl FirstPersonHands {
    /// Advance both hands by one fixed tick. See the module doc for the rule.
    pub(crate) fn tick(&mut self, input: HandTickInput<'_>) {
        self.hands_busy = input.hands_busy;
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

    /// A use of the item in `hand` succeeded: drop that hand to the bottom so
    /// the following ticks raise it back into view. Leaves the other hand, the
    /// shown stacks and the arm swing alone.
    ///
    /// The previous height goes to `0` as well: the reference client makes
    /// this call in the same tick, ahead of the hand step that overwrites the
    /// previous height, so the hand is *at* the bottom for the rest of the
    /// tick rather than easing down to it between frames.
    pub(crate) fn item_used(&mut self, hand: Hand) {
        let hand = match hand {
            Hand::Main => &mut self.main,
            Hand::Off => &mut self.off,
        };
        hand.height = 0.0;
        hand.previous = 0.0;
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
        let main_item = main.as_ref().map(|stack| stack.item().clone());
        if self.first_person_hands.last_main_item != main_item {
            self.first_person_hands.last_main_item = main_item;
            self.reset_attack_strength_ticker();
        }
        let hands_busy = self.paddling();
        let input = HandTickInput {
            main: main.as_ref(),
            off: off.as_ref(),
            hands_busy,
            main_swap_scale: self.item_swap_scale(),
        };
        self.first_person_hands.tick(input);
    }

    /// Controlling a boat with a movement key held: both hands are on the
    /// oars. Read from this tick's movement intent, the same bits the boat's
    /// own paddle input is built from.
    fn paddling(&self) -> bool {
        let boat = self.read(|w| {
            w.get_resource::<lodestone_ecs::vehicle::ControlledVehicle>()
                .and_then(|held| held.0.as_ref())
                .is_some_and(|held| held.family == lodestone_ecs::vehicle::VehicleFamily::Boat)
        });
        if !boat {
            return false;
        }
        let intent = self.movement_intent();
        intent.forward != 0.0 || intent.strafe != 0.0
    }

    /// Whether the hands were busy (paddling) on the last tick. Attack and use
    /// clicks do nothing while they are.
    #[must_use]
    pub(crate) fn hands_busy(&self) -> bool {
        self.first_person_hands.hands_busy
    }

    /// A use of the item in `hand` succeeded: snap that hand down so it rises
    /// back into view over the next ticks.
    pub(crate) fn first_person_item_used(&mut self, hand: Hand) {
        self.first_person_hands.item_used(hand);
    }

    /// The main hand's cooldown scale at the next tick:
    /// `clamp((item_swap_ticks + 1) / attack_delay, 0, 1)`, where the delay is
    /// `20 / attack_speed` ticks (the same delay the crosshair indicator
    /// divides by). Cubed, it is the main hand's resting height, so right
    /// after an attack the held item sits low and rises back as the cooldown
    /// recovers.
    #[must_use]
    pub(crate) fn item_swap_scale(&self) -> f32 {
        let ticks = self.read(|w| {
            w.get::<lodestone_ecs::ItemSwapTicker>(self.local)
                .map_or(0, |ticker| ticker.0)
        });
        ((ticks as f32 + 1.0) / self.attack_strength_delay()).clamp(0.0, 1.0)
    }

    /// Restart **both** attack counters: the crosshair cooldown and the held
    /// item's lowering. An entity attack, a swing at nothing, an abandoned dig
    /// and a change of main-hand item all do this.
    pub(crate) fn reset_attack_strength_ticker(&mut self) {
        let local = self.local;
        self.write(|w| {
            if let Some(mut ticker) = w.get_mut::<lodestone_ecs::AttackStrengthTicker>(local) {
                ticker.0 = 0;
            }
            if let Some(mut ticker) = w.get_mut::<lodestone_ecs::ItemSwapTicker>(local) {
                ticker.0 = 0;
            }
        });
    }

    /// Restart only the crosshair cooldown, leaving the held item where it is
    /// — a piercing weapon's attack.
    pub(crate) fn reset_only_attack_strength_ticker(&mut self) {
        let local = self.local;
        self.write(|w| {
            if let Some(mut ticker) = w.get_mut::<lodestone_ecs::AttackStrengthTicker>(local) {
                ticker.0 = 0;
            }
        });
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

    /// The main hand's resting height after an attack is the cubed cooldown
    /// scale. With a `1.6` attack speed the delay is `20 / 1.6 = 12.5` ticks,
    /// and `k` ticks after the reset the scale is `(k + 1) / 12.5`; stepping
    /// from `1.0` toward its cube by at most `0.4` a tick gives
    /// `0.6, 0.2, 0.032768 (= 0.32³), 0.064 (= 0.4³), 0.110592, …` and back
    /// to `1.0` on the twelfth tick, when the scale saturates. The off hand
    /// ignores the cooldown entirely.
    #[test]
    fn the_main_hand_follows_the_cubed_cooldown_after_an_attack() {
        let sword = stack("minecraft:iron_sword", 1);
        let shield = stack("minecraft:shield", 1);
        let mut hands = settled(Some(&sword), Some(&shield));
        let want = [
            0.6, 0.2, 0.032768, 0.064, 0.110592, 0.175616, 0.262144, 0.373248, 0.512, 0.681472,
            0.884736, 1.0, 1.0,
        ];
        for (k, want) in want.iter().enumerate() {
            let scale = ((k as f32 + 2.0) / 12.5).min(1.0);
            hands.tick(HandTickInput {
                main: Some(&sword),
                off: Some(&shield),
                hands_busy: false,
                main_swap_scale: scale,
            });
            assert!(
                (hands.main.height - want).abs() < 1e-5,
                "tick {} after the reset: expected {want}, got {}",
                k + 1,
                hands.main.height
            );
            assert_eq!(hands.off.height, 1.0, "the off hand ignores the cooldown");
            assert_eq!(hands.main.shown.as_ref(), Some(&sword), "a cooldown is not a swap");
        }
    }

    /// A successful use snaps the used hand to the bottom — at once, at any
    /// partial tick — and the ordinary step raises it: `0.4, 0.8, 1.0`. The
    /// other hand and both shown stacks are untouched.
    #[test]
    fn a_used_item_snaps_down_and_rises_back() {
        let bread = stack("minecraft:bread", 5);
        let shield = stack("minecraft:shield", 1);
        let mut hands = settled(Some(&bread), Some(&shield));
        hands.item_used(Hand::Main);
        let (main, off) = hands.sample(0.5);
        assert_eq!(main.inverse_arm_height, 1.0, "the snap is immediate, not eased");
        assert_eq!(off.inverse_arm_height, 0.0, "the other hand stays up");
        assert_eq!(main.shown.as_ref(), Some(&bread));
        for want in [0.4, 0.8, 1.0, 1.0] {
            hands.tick(HandTickInput {
                main: Some(&bread),
                off: Some(&shield),
                hands_busy: false,
                main_swap_scale: 1.0,
            });
            assert!((hands.main.height - want).abs() < 1e-6, "{want} vs {}", hands.main.height);
            assert_eq!(hands.off.height, 1.0);
        }
    }

    /// While the hands are busy both fall by `0.4` a tick to `0` whatever is
    /// held, and stay there; once they are free again both rise by the
    /// ordinary rule. The stacks themselves never change.
    #[test]
    fn busy_hands_lower_both_and_freeing_them_raises_both() {
        let sword = stack("minecraft:iron_sword", 1);
        let shield = stack("minecraft:shield", 1);
        let mut hands = settled(Some(&sword), Some(&shield));
        let tick = |hands: &mut FirstPersonHands, busy: bool| {
            hands.tick(HandTickInput {
                main: Some(&sword),
                off: Some(&shield),
                hands_busy: busy,
                main_swap_scale: 1.0,
            });
            (hands.main.height, hands.off.height)
        };
        for want in [0.6, 0.2, 0.0, 0.0] {
            let (main, off) = tick(&mut hands, true);
            assert!((main - want).abs() < 1e-6 && (off - want).abs() < 1e-6, "{want}: {main} {off}");
        }
        for want in [0.4, 0.8, 1.0] {
            let (main, off) = tick(&mut hands, false);
            assert!((main - want).abs() < 1e-6 && (off - want).abs() < 1e-6, "{want}: {main} {off}");
        }
        assert_eq!(hands.main.shown.as_ref(), Some(&sword));
        assert_eq!(hands.off.shown.as_ref(), Some(&shield));
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
