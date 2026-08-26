//! Eating and drinking, client side: the crumbs and the state the first-person
//! bob reads.
//!
//! # What it is
//!
//! The server owns *what eating does* (nutrition, saturation, the use clock,
//! cancel-on-release — `lodestone_server::item_use`). This module owns the two
//! client-visible halves vanilla's `Consumable.emitParticlesAndSounds` and
//! `ItemInHandRenderer.applyEatTransform` produce, and neither is a guess about
//! server state: both are derived from the local player's own use, exactly as
//! vanilla's client derives them.
//!
//! | half | where |
//! |---|---|
//! | the `ITEM` crumbs, on vanilla's cadence | [`emit_consume_particles`], a `GameTick` system |
//! | the first-person dip/jitter | [`ConsumeState`] → `RenderState::set_item_use_source` → `lodestone_render::entity::first_person_eat_matrix` |
//! | the third-person raised arm | nothing here — it is the *remote* entity path, `entities::arm_pose_for`'s `ArmPose::Item` |
//! | the sound | nothing here — the **server** broadcasts it; see below |
//!
//! # Why the sound is not on this side
//!
//! Vanilla runs `Consumable.emitParticlesAndSounds` on both sides and each drops
//! the half it cannot do: `ServerLevel.addParticle` is a no-op, and
//! `ClientLevel.playSeededSound` skips a sound whose excluded player is not the
//! local one — which `Entity.playSound`'s `playSound(null, …)` always satisfies. So
//! particles are *always* predicted and the eating sound is *always* the server's
//! broadcast. Emitting the sound here too would double it against a real 26.2
//! server, which is the same trap `docs/sound-playback.md` records for block
//! breaking pointing the other way. The integrated server's half is
//! `lodestone_server`'s `WorldEffect::Sound` publisher.
//!
//! # How it works
//!
//! [`ConsumeState::resolve`] is the whole decision, and it is a **named
//! composition** rather than a condition spread over its two consumers: the
//! particles and the bob must agree tick for tick about whether a consume is in
//! progress, and a bug in the seam between two individually-correct halves has no
//! subject for a test to point at otherwise.
//!
//! It joins five things:
//!
//! 1. [`UsingItem`](crate::interact::UsingItem) — the use button is down.
//! 2. [`ItemUseTicks`] — how long, in 20 Hz ticks, counting **up**.
//! 3. the selected hotbar item's registry name.
//! 4. that item's `minecraft:consumable` component
//!    ([`lodestone_game::consumable`]).
//! 5. for a `minecraft:food` item, the hunger gate
//!    ([`lodestone_game::food::can_eat`]) — a full, non-invulnerable player's
//!    use of an ordinary food resolves to `None` here, the same `FAIL`
//!    vanilla's server-side `Player.canEat` gives it, rather than the whole
//!    bite animation and crumbs for a use the server was always going to
//!    refuse.
//!
//! and then bounds the result by the item's own `consume_ticks`, which is what
//! makes the animation stop on its own once a use the gates above *did* allow
//! finishes. That bound is load-bearing on its own axis: `Sim::use_item_live`
//! arms `ItemUseTicks` on the **press edge for any item that can enter vanilla's
//! use state**, including a non-consumable use that has no consume animation.
//! Without the bound, holding the button after a finished bite would animate
//! and throw crumbs forever.
//!
//! # How to change it, and the gotchas
//!
//! * **A drink is not a food.** `has_consume_particles` is false for all four
//!   drinks, so the particle system must consult it rather than the animation. A
//!   potion that throws crumbs passes every "are there particles" check.
//! * **The item's own texture is the point.** The crumbs carry
//!   `SpriteSource::Item(id)` and the shell resolves that against the baked item
//!   models. A generic crumb satisfies any presence check and is visibly wrong for
//!   anything coloured.
//! * **The off-hand is not modelled.** `ItemUseTicks` and `UsingItem` are both
//!   hand-free scalars and the shell's own held-item render path reads the *main*
//!   hand only, so drinking from the off hand animates the main hand's item. Fixing
//!   it needs a hand on the use state, not more work here.
//! * **Do not reach for a clock.** Every duration here is in ticks;
//!   `SystemTime::now`/`Instant::now` trap on wasm32.

use lodestone_ecs::ecs::prelude::{Query, Res, ResMut, With};
use lodestone_ecs::player::{ItemUseTicks, LocalPlayer, PhysicsState, SelectedSlot};
use lodestone_ecs::session::{ServerGameMode, SessionMenus, Vitals};
use lodestone_game::consumable::{self, ConsumeAnimation, Consumable};

use crate::interact::{ParticleSim, UsingItem};

/// An in-progress eat or drink by the **local** player.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ConsumeState {
    /// Network registry id of the item being consumed —
    /// `SpriteSource::Item`'s key, so the crumbs carry the food's own texture.
    pub item_id: u32,
    /// The item's `minecraft:consumable` component.
    pub consumable: Consumable,
    /// Ticks elapsed since the use began, vanilla's `getTicksUsingItem()`.
    /// Always strictly less than `consumable.consume_ticks`.
    pub ticks_used: u32,
}

impl ConsumeState {
    /// The join described in the [module docs](self): `None` unless the use button
    /// is down, an item-use clock is running, the selected item is consumable, the
    /// clock has not run past that item's duration, and — for an item that is
    /// `minecraft:food` — the hunger gate [`lodestone_game::food::can_eat`] allows
    /// it.
    ///
    /// A free function over its inputs rather than a `Sim` method, so both the
    /// `GameTick` system and the render source resolve the *same* expression and a
    /// test can drive it without a world.
    ///
    /// # The food-full-hunger gate
    ///
    /// [`consumable::consumable_for_item`] alone answers "does this item animate
    /// as eat/drink at all" — every `minecraft:food` item passes it regardless of
    /// hunger, which is why a full player used to see the whole bite animation and
    /// crumbs for a food the server was about to refuse with `FAIL`.
    /// [`lodestone_game::food::always_eat_for_food`] narrows to the 40 items that
    /// are actually `minecraft:food`; `None` (a drink, or anything non-food) means
    /// no hunger gate applies at all, matching vanilla's `Consumable.startConsuming`
    /// only checking `canEat` for food in the first place. `food_level: None`
    /// (the server has not told us yet) does **not** gate — an unknown hunger
    /// level must never block a use this prediction cannot actually verify either
    /// way, and the server's own refusal is still the authority if this guesses
    /// wrong.
    #[must_use]
    pub fn resolve(
        using: bool,
        ticks_used: Option<u32>,
        item: Option<&str>,
        food_level: Option<i32>,
        invulnerable: bool,
    ) -> Option<Self> {
        if !using {
            return None;
        }
        let ticks_used = ticks_used?;
        let item = item?;
        let consumable = consumable::consumable_for_item(item)?;
        if let Some(always_eat) = lodestone_game::food::always_eat_for_food(item)
            && let Some(level) = food_level
            && !lodestone_game::food::can_eat(always_eat, level, invulnerable)
        {
            return None;
        }
        // The bound that makes the animation self-terminating — see the module
        // docs. `>=` and not `>`: at `ticks_used == consume_ticks` vanilla's
        // `useItemRemaining` has reached 0 and `completeUsingItem` has run.
        if ticks_used >= consumable.consume_ticks {
            return None;
        }
        let item_id = u32::try_from(lodestone_data::items::item_id(item)?).ok()?;
        Some(Self {
            item_id,
            consumable,
            ticks_used,
        })
    }

    /// `LivingEntity.getUseItemRemainingTicks()` — what every vanilla consume
    /// expression is written in terms of.
    #[must_use]
    pub fn remaining_ticks(&self) -> u32 {
        consumable::remaining_ticks(self.consumable.consume_ticks, self.ticks_used)
    }

    /// Whether this tick is one of the six (for a default food) on which
    /// `ItemStack.onUseTick` fires effects — **and** whether this item has
    /// particles at all.
    ///
    /// The conjunction is here rather than at the call site because the two
    /// questions are asked together everywhere and separating them is how a potion
    /// ends up throwing crumbs.
    #[must_use]
    pub fn emits_particles_this_tick(&self) -> bool {
        self.consumable.has_consume_particles
            && consumable::should_emit_consume_effects(
                self.consumable.consume_ticks,
                self.remaining_ticks(),
            )
    }

    /// `true` for `ItemUseAnimation.DRINK`. Kept as an accessor because the
    /// animation is *not* what selects the pose (eat and drink share one transform)
    /// — only the sound and the particle flag differ, and reading `animation` at a
    /// pose site is the mistake that invites.
    #[must_use]
    pub fn is_drink(&self) -> bool {
        self.consumable.animation == ConsumeAnimation::Drink
    }
}

/// `ItemStack.onUseTick` → `Consumable.emitParticlesAndSounds` →
/// `LivingEntity.spawnItemParticles(stack, 5)`, for the local player.
///
/// Runs in `TickSet::Send`'s chain next to `drive_mining` because it shares
/// [`ParticleSim`] with it and this app runs with
/// `ambiguity_detection: LogLevel::Error`; the position within the chain is not
/// otherwise meaningful.
///
/// # Ticks, not frames
///
/// The cadence is `remaining % 4 == 0`, so it must be evaluated exactly once per
/// 20 Hz tick. Driving it from the render loop instead would emit at the frame rate
/// and turn six crumb bursts into hundreds — a difference that reads as "the
/// particle count is wrong" rather than as a scheduling error.
pub fn emit_consume_particles(
    using: Res<UsingItem>,
    ticks: Res<ItemUseTicks>,
    mut particles: ResMut<ParticleSim>,
    players: Query<
        (&PhysicsState, &SelectedSlot, &SessionMenus, &Vitals, &ServerGameMode),
        With<LocalPlayer>,
    >,
) {
    let Ok((state, slot, menus, vitals, game_mode)) = players.single() else {
        return;
    };
    let held = menus
        .0
        .player()
        .player_native(slot.0)
        .map(|stack| stack.item().to_string());
    // `!crate::hud::can_hurt_player`, not a second creative/spectator check —
    // vanilla's `abilities.invulnerable` and `MultiPlayerGameMode.
    // canHurtPlayer()` agree on exactly the same two game modes.
    let invulnerable = !crate::hud::can_hurt_player(game_mode.0);
    let Some(consume) =
        ConsumeState::resolve(using.0, ticks.0, held.as_deref(), vitals.food, invulnerable)
    else {
        return;
    };
    if !consume.emits_particles_this_tick() {
        return;
    }
    let pos = state.0.position;
    lodestone_particle::emit::spawn_item_particles(
        particles.0.engine_mut(),
        pos.x,
        // `getEyeY()`, which is what `spawnItemParticles` offsets from; the crumbs
        // then spawn 0.3..0.9 blocks *below* it, where a mouth is.
        pos.y + f64::from(state.0.eye_height),
        pos.z,
        state.0.pitch,
        state.0.yaw,
        consume.item_id,
        consumable::PERIODIC_PARTICLE_COUNT,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`ConsumeState::resolve`] with the food-hunger-gate pair defaulted to
    /// "no gate applies" (`food_level: None, invulnerable: false`) — every
    /// test that predates the hunger gate and is not itself testing it uses
    /// this, so the four-conjunct/duration/particle/id tests below are
    /// unaffected by the fifth conjunct this module added.
    fn resolve(using: bool, ticks_used: Option<u32>, item: Option<&str>) -> Option<ConsumeState> {
        ConsumeState::resolve(using, ticks_used, item, None, false)
    }

    /// The five-way join, arm by arm. Each row is a single missing conjunct, so a
    /// resolver that dropped one would pass the others and fail exactly here.
    #[test]
    fn a_consume_needs_all_four_conjuncts() {
        let ok = resolve(true, Some(4), Some("minecraft:carrot"));
        assert!(ok.is_some(), "the positive case must resolve");
        assert_eq!(resolve(false, Some(4), Some("minecraft:carrot")), None);
        assert_eq!(resolve(true, None, Some("minecraft:carrot")), None);
        assert_eq!(resolve(true, Some(4), None), None);
        // Held, in use, clock running — and not edible. This is the arm that fires
        // on every right-click with a pickaxe, so it is the one that matters most.
        assert_eq!(
            resolve(true, Some(4), Some("minecraft:diamond_pickaxe")),
            None
        );
    }

    /// The duration bound. `Sim::use_item_live` arms the clock on the press edge
    /// whatever is held, and the server may have refused the use entirely, so the
    /// animation has to end itself.
    #[test]
    fn a_consume_ends_at_its_own_duration() {
        let carrot = "minecraft:carrot";
        assert!(resolve(true, Some(31), Some(carrot)).is_some());
        assert_eq!(resolve(true, Some(32), Some(carrot)), None);
        assert_eq!(resolve(true, Some(1_000), Some(carrot)), None);
        // Dried kelp is 16 ticks, so the bound is per item and not a constant: the
        // tick that is still eating a carrot has already finished the kelp.
        assert!(resolve(true, Some(15), Some("minecraft:dried_kelp")).is_some());
        assert_eq!(
            resolve(true, Some(16), Some("minecraft:dried_kelp")),
            None
        );
    }

    /// The particle cadence and the count over a whole use, predicted from the
    /// consumable data rather than observed — and the drink arm, which must emit
    /// **zero** over the same span.
    #[test]
    fn a_food_emits_six_bursts_and_a_drink_none() {
        let bursts = |item: &str, duration: u32| {
            (0..duration)
                .filter(|&t| {
                    resolve(true, Some(t), Some(item)).is_some_and(|c| c.emits_particles_this_tick())
                })
                .count()
        };
        // Six bursts of five crumbs, the count `lodestone_game::consumable`'s own
        // gate derives from the interval and the start fraction.
        assert_eq!(bursts("minecraft:carrot", 32), 6);
        assert_eq!(bursts("minecraft:dried_kelp", 16), 3);
        // Every drink: `hasConsumeParticles(false)`. A potion that throws crumbs
        // passes any presence check, which is why this is asserted at zero rather
        // than left to the flag.
        assert_eq!(bursts("minecraft:potion", 32), 0);
        assert_eq!(bursts("minecraft:milk_bucket", 32), 0);
        assert_eq!(bursts("minecraft:honey_bottle", 40), 0);
    }

    /// The crumbs must carry the *eaten* item, not a generic one. Two foods with
    /// visibly different sprites, asserted to resolve to two different ids.
    #[test]
    fn the_crumbs_carry_the_eaten_items_own_id() {
        let carrot = resolve(true, Some(8), Some("minecraft:carrot")).expect("carrot");
        let beetroot = resolve(true, Some(8), Some("minecraft:beetroot")).expect("beetroot");
        assert_ne!(
            carrot.item_id, beetroot.item_id,
            "an orange crumb and a red one must come from different item ids"
        );
        assert_eq!(
            u32::try_from(lodestone_data::items::item_id("minecraft:carrot").expect("registered"))
                .expect("non-negative"),
            carrot.item_id,
            "the id must be the registry id the sprite table is indexed by"
        );
    }

    #[test]
    fn eat_and_drink_share_one_pose_but_differ_in_sound() {
        let carrot = resolve(true, Some(8), Some("minecraft:carrot")).expect("carrot");
        let potion = resolve(true, Some(8), Some("minecraft:potion")).expect("potion");
        assert!(!carrot.is_drink());
        assert!(potion.is_drink());
        assert_eq!(carrot.consumable.sound, consumable::EAT_SOUND);
        assert_eq!(potion.consumable.sound, consumable::DRINK_SOUND);
        // Same duration, so the same `remaining_ticks` at the same tick — the pose
        // really is shared.
        assert_eq!(carrot.remaining_ticks(), potion.remaining_ticks());
    }

    /// The hunger gate itself, this issue's own discriminating pair: a plain
    /// apple at a full bar must not resolve at all (the animation this
    /// module drives must never start for a use the server will `FAIL`), a
    /// golden apple at the same full bar must. Two plain foods would coincide
    /// on both hypotheses; this is why the pair is a golden apple and a plain
    /// one, not two ordinary foods.
    #[test]
    fn a_full_bar_refuses_a_plain_apple_but_not_a_golden_one() {
        let full = lodestone_game::food::MAX_FOOD;
        assert_eq!(
            ConsumeState::resolve(true, Some(0), Some("minecraft:apple"), Some(full), false),
            None,
            "a full, non-invulnerable player must not start eating a plain apple"
        );
        assert!(
            ConsumeState::resolve(true, Some(0), Some("minecraft:golden_apple"), Some(full), false)
                .is_some(),
            "a golden apple bypasses a full bar"
        );
        // The hungry control: the exact same apple, one point short of full,
        // must resolve — proving the refusal above is the food gate and not
        // some other broken conjunct.
        assert!(
            ConsumeState::resolve(true, Some(0), Some("minecraft:apple"), Some(full - 1), false)
                .is_some(),
            "a hungry player may still eat a plain apple"
        );
        // Invulnerable (creative/spectator) bypasses the gate for any food.
        assert!(
            ConsumeState::resolve(true, Some(0), Some("minecraft:apple"), Some(full), true).is_some(),
            "an invulnerable player may eat a plain apple at a full bar"
        );
        // An unknown food level (`None`, before the server has told us) must
        // not block — this prediction cannot verify the gate either way, and
        // the server remains authoritative regardless.
        assert!(
            ConsumeState::resolve(true, Some(0), Some("minecraft:apple"), None, false).is_some(),
            "an unknown food level must not gate the prediction"
        );
        // A drink has no food component and so no hunger gate at all, even
        // at a full bar.
        assert!(
            ConsumeState::resolve(true, Some(0), Some("minecraft:potion"), Some(full), false)
                .is_some(),
            "a drink is never gated on hunger"
        );
    }
}
