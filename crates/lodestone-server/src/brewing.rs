//! Brewing stand block entity: the ingredient -> potion transition table and
//! brew/fuel state machine.
//!
//! # Where the truth comes from
//!
//! The decompiled brewing-stand block entity (the state machine) and the
//! decompiled potion-brewing recipe registry (**not** a "recipe registry" name —
//! that name does not exist in this decompile; verified directly rather than
//! assumed).
//!
//! * Brew time is a **bare literal**, not a named constant: the real tick handler
//!   assigns `400` directly to the brew-time counter. A separate
//!   `BREWING_TIME_SECONDS = 20` constant exists but is never referenced or
//!   multiplied anywhere in the class — confirmed by grep, not assumed — so it is
//!   purely documentary and [`BREW_TIME_TICKS`] restates the real literal (`400`,
//!   i.e. 20 real seconds at 20 ticks/s, which is presumably what that unused
//!   constant was *meant* to document) rather than deriving from it.
//! * The real fuel-uses constant, `20`, **is** the real value used: the fuel
//!   counter is set to 20 when the fuel slot holds blaze powder and the charge
//!   counter is empty. One blaze powder = 20 brews; the charge decrements once per
//!   brew *started* (in the same tick handler), not per tick.
//! * The real per-tick rule, restated as the model for [`BrewingStand::tick`]:
//!   if the fuel counter is at or below zero and the fuel slot holds a
//!   brewing-fuel item, the counter resets to 20 and the fuel item shrinks by one.
//!   Then, if the current ingredient/bottle combination is brewable and the brew
//!   counter is positive, the counter decrements; if it just hit zero and the
//!   combination is still brewable, the brew completes, and otherwise — if the
//!   combination stopped being brewable, or the ingredient slot's item changed
//!   from what started the brew — the counter is reset to zero, aborting it.
//!   Otherwise, if not already brewing and the combination is brewable and fuel
//!   remains, a new brew starts: fuel decrements, the brew counter is set to 400,
//!   and the started ingredient item is captured.
//!   The ingredient-changed abort check compares the *current* ingredient-slot
//!   item against whichever item was captured the moment this brew started —
//!   swapping the ingredient mid-brew cancels it, even with plenty of brew time
//!   left.
//! * The real complete-brew rule applies the mix function to **all three** bottle
//!   slots unconditionally (an empty or non-matching slot is a no-op inside the
//!   mix function itself — see [`mix_bottle`]), then shrinks the ingredient by 1.
//! * **Splash/lingering is not a separate post-processing step** — this was
//!   the vanilla belief specifically worth re-checking (CLAUDE.md's
//!   "re-verify before routing around" rule): the real mix rule
//!   checks the container-promotion table (gunpowder: potion ->
//!   splash potion; dragon's breath: splash potion -> lingering potion,
//!   registered alongside every other vanilla mix) **before** falling through
//!   to the potion-type table, in the exact same 400-tick
//!   brew cycle as any other ingredient — gunpowder simply occupies the
//!   ingredient slot for one ordinary brew like anything else.
//!
//! ## What this module does not model
//!
//! * **A real `ItemStack`.** `lodestone_model::ItemStack` has no potion
//!   contents component at all (checked directly: `ItemComponents` models
//!   `custom_name`/`damage`/`enchantments`/`dyed_color`/`tool`/
//!   `max_stack_size`/`max_damage`/`equippable`/`has_unmodeled`, nothing
//!   potion-shaped) — adding one is a shared-model change well outside this
//!   issue's file ownership. [`Bottle`] is this module's own minimal stand-in
//!   (container kind + potion id string) rather than a real `ItemStack`; see
//!   the top-level report for this as a declared, named gap.
//! * **Empty glass bottles sitting in a bottle slot.** Vanilla allows it (the
//!   real slot-acceptance rule for slots 0-2 also accepts a plain glass bottle),
//!   but the mix function is a no-op against one regardless (a glass bottle has no
//!   potion-contents component, so both "has a potion mix" and "has a container
//!   mix" are trivially false) — modeling it would add a variant that never
//!   does anything, so a bottle slot here is `Option<Bottle>` and empty
//!   means "nothing here at all," not "an empty bottle."
//! * **Ingredient item validation beyond the mix table itself.** Vanilla's
//!   real slot-acceptance rule for slot 3 calls the same is-ingredient check
//!   this module's [`is_ingredient`] performs — no separate allow-list.

/// The literal `400` from the real per-tick rule — see the module doc comment
/// for why this is not derived from the unused `BREWING_TIME_SECONDS` constant.
pub const BREW_TIME_TICKS: i32 = 400;

/// The real fuel-uses constant, and the value the real per-tick rule
/// actually assigns — one blaze powder charges 20 brews.
pub const FUEL_USES: i32 = 20;

/// Which of the three bottle-item kinds a [`Bottle`] is — determines which
/// container-promotion entry (if any) can promote it (from the real
/// vanilla-mixes registration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BottleKind {
    Potion,
    Splash,
    Lingering,
}

/// A brewing-stand bottle slot's contents: a container kind plus the potion
/// id it currently holds (e.g. `"minecraft:water"`, `"minecraft:awkward"`,
/// `"minecraft:swiftness"`) — see the module doc comment for why this is a
/// dedicated small type rather than a real `ItemStack`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bottle {
    pub kind: BottleKind,
    pub potion: String,
}

impl Bottle {
    #[must_use]
    pub fn new(kind: BottleKind, potion: impl Into<String>) -> Self {
        Self {
            kind,
            potion: potion.into(),
        }
    }
}

/// The container-promotion table, transcribed from the real vanilla-mixes
/// registration's container-recipe calls for gunpowder and dragon's breath:
/// gunpowder promotes a `Potion` bottle to `Splash`; dragon's breath promotes a
/// `Splash` bottle to `Lingering`. Checked before [`potion_mix`], matching the
/// real mix rule's own order.
#[must_use]
fn container_mix(kind: BottleKind, ingredient: &str) -> Option<BottleKind> {
    match (kind, ingredient) {
        (BottleKind::Potion, "minecraft:gunpowder") => Some(BottleKind::Splash),
        (BottleKind::Splash, "minecraft:dragon_breath") => Some(BottleKind::Lingering),
        _ => None,
    }
}

/// The potion-type transition table, transcribed directly from the real
/// vanilla-mixes registration — every mix and start-mix registration, with each
/// start-mix registration expanded to its documented pair (`WATER + ingredient ->
/// MUNDANE`, `AWKWARD + ingredient -> potion`).
#[must_use]
#[allow(clippy::too_many_lines)]
fn potion_mix(from: &str, ingredient: &str) -> Option<&'static str> {
    match (from, ingredient) {
        ("minecraft:water", "minecraft:glowstone_dust") => Some("minecraft:thick"),
        ("minecraft:water", "minecraft:redstone") => Some("minecraft:mundane"),
        ("minecraft:water", "minecraft:nether_wart") => Some("minecraft:awkward"),
        // addStartMix(BREEZE_ROD, WIND_CHARGED)
        ("minecraft:water", "minecraft:breeze_rod") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:breeze_rod") => Some("minecraft:wind_charged"),
        // addStartMix(SLIME_BLOCK, OOZING)
        ("minecraft:water", "minecraft:slime_block") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:slime_block") => Some("minecraft:oozing"),
        // addStartMix(STONE, INFESTED)
        ("minecraft:water", "minecraft:stone") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:stone") => Some("minecraft:infested"),
        // addStartMix(COBWEB, WEAVING)
        ("minecraft:water", "minecraft:cobweb") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:cobweb") => Some("minecraft:weaving"),

        ("minecraft:awkward", "minecraft:golden_carrot") => Some("minecraft:night_vision"),
        ("minecraft:night_vision", "minecraft:redstone") => Some("minecraft:long_night_vision"),
        ("minecraft:night_vision", "minecraft:fermented_spider_eye") => Some("minecraft:invisibility"),
        ("minecraft:long_night_vision", "minecraft:fermented_spider_eye") => {
            Some("minecraft:long_invisibility")
        }
        ("minecraft:invisibility", "minecraft:redstone") => Some("minecraft:long_invisibility"),

        // addStartMix(MAGMA_CREAM, FIRE_RESISTANCE)
        ("minecraft:water", "minecraft:magma_cream") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:magma_cream") => Some("minecraft:fire_resistance"),
        ("minecraft:fire_resistance", "minecraft:redstone") => Some("minecraft:long_fire_resistance"),

        // addStartMix(RABBIT_FOOT, LEAPING)
        ("minecraft:water", "minecraft:rabbit_foot") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:rabbit_foot") => Some("minecraft:leaping"),
        ("minecraft:leaping", "minecraft:redstone") => Some("minecraft:long_leaping"),
        ("minecraft:leaping", "minecraft:glowstone_dust") => Some("minecraft:strong_leaping"),
        ("minecraft:leaping", "minecraft:fermented_spider_eye") => Some("minecraft:slowness"),
        ("minecraft:long_leaping", "minecraft:fermented_spider_eye") => Some("minecraft:long_slowness"),
        ("minecraft:slowness", "minecraft:redstone") => Some("minecraft:long_slowness"),
        ("minecraft:slowness", "minecraft:glowstone_dust") => Some("minecraft:strong_slowness"),

        ("minecraft:awkward", "minecraft:turtle_helmet") => Some("minecraft:turtle_master"),
        ("minecraft:turtle_master", "minecraft:redstone") => Some("minecraft:long_turtle_master"),
        ("minecraft:turtle_master", "minecraft:glowstone_dust") => Some("minecraft:strong_turtle_master"),

        ("minecraft:swiftness", "minecraft:fermented_spider_eye") => Some("minecraft:slowness"),
        ("minecraft:long_swiftness", "minecraft:fermented_spider_eye") => Some("minecraft:long_slowness"),
        // addStartMix(SUGAR, SWIFTNESS)
        ("minecraft:water", "minecraft:sugar") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:sugar") => Some("minecraft:swiftness"),
        ("minecraft:swiftness", "minecraft:redstone") => Some("minecraft:long_swiftness"),
        ("minecraft:swiftness", "minecraft:glowstone_dust") => Some("minecraft:strong_swiftness"),

        ("minecraft:awkward", "minecraft:pufferfish") => Some("minecraft:water_breathing"),
        ("minecraft:water_breathing", "minecraft:redstone") => Some("minecraft:long_water_breathing"),

        // addStartMix(GLISTERING_MELON_SLICE, HEALING)
        ("minecraft:water", "minecraft:glistering_melon_slice") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:glistering_melon_slice") => Some("minecraft:healing"),
        ("minecraft:healing", "minecraft:glowstone_dust") => Some("minecraft:strong_healing"),
        ("minecraft:healing", "minecraft:fermented_spider_eye") => Some("minecraft:harming"),
        ("minecraft:strong_healing", "minecraft:fermented_spider_eye") => Some("minecraft:strong_harming"),
        ("minecraft:harming", "minecraft:glowstone_dust") => Some("minecraft:strong_harming"),

        ("minecraft:poison", "minecraft:fermented_spider_eye") => Some("minecraft:harming"),
        ("minecraft:long_poison", "minecraft:fermented_spider_eye") => Some("minecraft:harming"),
        ("minecraft:strong_poison", "minecraft:fermented_spider_eye") => Some("minecraft:strong_harming"),
        // addStartMix(SPIDER_EYE, POISON)
        ("minecraft:water", "minecraft:spider_eye") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:spider_eye") => Some("minecraft:poison"),
        ("minecraft:poison", "minecraft:redstone") => Some("minecraft:long_poison"),
        ("minecraft:poison", "minecraft:glowstone_dust") => Some("minecraft:strong_poison"),

        // addStartMix(GHAST_TEAR, REGENERATION)
        ("minecraft:water", "minecraft:ghast_tear") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:ghast_tear") => Some("minecraft:regeneration"),
        ("minecraft:regeneration", "minecraft:redstone") => Some("minecraft:long_regeneration"),
        ("minecraft:regeneration", "minecraft:glowstone_dust") => Some("minecraft:strong_regeneration"),

        // addStartMix(BLAZE_POWDER, STRENGTH)
        ("minecraft:water", "minecraft:blaze_powder") => Some("minecraft:mundane"),
        ("minecraft:awkward", "minecraft:blaze_powder") => Some("minecraft:strength"),
        ("minecraft:strength", "minecraft:redstone") => Some("minecraft:long_strength"),
        ("minecraft:strength", "minecraft:glowstone_dust") => Some("minecraft:strong_strength"),

        ("minecraft:water", "minecraft:fermented_spider_eye") => Some("minecraft:weakness"),
        ("minecraft:weakness", "minecraft:redstone") => Some("minecraft:long_weakness"),

        ("minecraft:awkward", "minecraft:phantom_membrane") => Some("minecraft:slow_falling"),
        ("minecraft:slow_falling", "minecraft:redstone") => Some("minecraft:long_slow_falling"),

        _ => None,
    }
}

/// Every ingredient item referenced anywhere in [`potion_mix`] — used by
/// [`is_ingredient`] to answer "is this item usable as *some* brewing
/// ingredient" without needing a specific bottle to check against (mirrors the
/// real is-potion-ingredient rule, which likewise only checks
/// ingredient membership, not a specific mix).
fn is_potion_ingredient(item: &str) -> bool {
    matches!(
        item,
        "minecraft:glowstone_dust"
            | "minecraft:redstone"
            | "minecraft:nether_wart"
            | "minecraft:breeze_rod"
            | "minecraft:slime_block"
            | "minecraft:stone"
            | "minecraft:cobweb"
            | "minecraft:golden_carrot"
            | "minecraft:fermented_spider_eye"
            | "minecraft:magma_cream"
            | "minecraft:rabbit_foot"
            | "minecraft:turtle_helmet"
            | "minecraft:sugar"
            | "minecraft:pufferfish"
            | "minecraft:glistering_melon_slice"
            | "minecraft:spider_eye"
            | "minecraft:ghast_tear"
            | "minecraft:blaze_powder"
            | "minecraft:phantom_membrane"
    )
}

fn is_container_ingredient(item: &str) -> bool {
    matches!(item, "minecraft:gunpowder" | "minecraft:dragon_breath")
}

/// The real is-ingredient rule: whether `item` is usable as an
/// ingredient at all, in either table.
#[must_use]
pub fn is_ingredient(item: &str) -> bool {
    is_container_ingredient(item) || is_potion_ingredient(item)
}

/// The real has-mix rule: whether `ingredient` has *some* effect
/// on this specific `bottle` (container promotion or potion-type change).
#[must_use]
pub fn has_mix(bottle: &Bottle, ingredient: &str) -> bool {
    container_mix(bottle.kind, ingredient).is_some() || potion_mix(&bottle.potion, ingredient).is_some()
}

/// The real mix rule: applies `ingredient` to `bottle`,
/// container promotion first, then potion-type change, returning the
/// bottle unchanged if neither applies (matching the real complete-brew rule
/// calling this unconditionally on every bottle slot).
#[must_use]
pub fn mix_bottle(bottle: &Bottle, ingredient: &str) -> Bottle {
    if let Some(new_kind) = container_mix(bottle.kind, ingredient) {
        return Bottle::new(new_kind, bottle.potion.clone());
    }
    if let Some(new_potion) = potion_mix(&bottle.potion, ingredient) {
        return Bottle::new(bottle.kind, new_potion);
    }
    bottle.clone()
}

/// What happened on one [`BrewingStand::tick`] call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BrewTick {
    /// A brew cycle started this tick (fuel charge consumed,
    /// [`BREW_TIME_TICKS`] armed).
    pub started: bool,
    /// A brew completed this tick: every bottle slot with a valid mix was
    /// transformed and the ingredient was consumed.
    pub brewed: bool,
    /// One fuel item (blaze powder) was consumed to refill the charge
    /// counter to [`FUEL_USES`].
    pub fuel_refilled: bool,
}

/// A brewing stand's fuel/brew state plus its three bottle slots and
/// ingredient slot — see the module doc comment for the vanilla citation
/// this ports.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BrewingStand {
    bottles: [Option<Bottle>; 3],
    /// `(item id, count)` — the ingredient slot (vanilla's slot 3).
    ingredient: Option<(String, u32)>,
    /// The fuel slot (vanilla's slot 4) — `(item id, count)`, expected to
    /// only ever hold blaze powder (the `minecraft:brewing_fuel` tag's sole
    /// member).
    fuel_item: Option<(String, u32)>,
    fuel_charges: i32,
    brew_time: i32,
    /// The ingredient item captured when the current brew started
    /// — compared against the live ingredient slot
    /// each tick to detect a mid-brew swap.
    locked_ingredient: Option<String>,
}

impl BrewingStand {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds a brewing stand from persisted state — see
    /// [`crate::furnace::Furnace::restore`] for why this is one total
    /// constructor rather than a setter per field.
    ///
    /// `locked_ingredient` is the ingredient captured when the current brew
    /// started, and it is **not** redundant with the live ingredient slot:
    /// [`tick`](Self::tick) compares the two to detect a mid-brew swap, so a
    /// load that dropped it would let a player swap the ingredient across a
    /// save/load boundary and keep the original brew's progress.
    #[must_use]
    pub fn restore(
        bottles: [Option<Bottle>; 3],
        ingredient: Option<(String, u32)>,
        fuel_item: Option<(String, u32)>,
        fuel_charges: i32,
        brew_time: i32,
        locked_ingredient: Option<String>,
    ) -> Self {
        Self {
            bottles,
            ingredient,
            fuel_item,
            fuel_charges,
            brew_time,
            locked_ingredient,
        }
    }

    /// The ingredient captured when the running brew started, for
    /// persistence. See [`restore`](Self::restore) for why it matters.
    #[must_use]
    pub fn locked_ingredient(&self) -> Option<&str> {
        self.locked_ingredient.as_deref()
    }

    #[must_use]
    pub fn bottle(&self, slot: usize) -> Option<&Bottle> {
        self.bottles.get(slot).and_then(Option::as_ref)
    }

    pub fn set_bottle(&mut self, slot: usize, bottle: Option<Bottle>) {
        if let Some(s) = self.bottles.get_mut(slot) {
            *s = bottle;
        }
    }

    #[must_use]
    pub fn ingredient(&self) -> Option<(&str, u32)> {
        self.ingredient.as_ref().map(|(i, c)| (i.as_str(), *c))
    }

    pub fn set_ingredient(&mut self, item: Option<(String, u32)>) {
        self.ingredient = item;
    }

    #[must_use]
    pub fn fuel_item(&self) -> Option<(&str, u32)> {
        self.fuel_item.as_ref().map(|(i, c)| (i.as_str(), *c))
    }

    pub fn set_fuel_item(&mut self, item: Option<(String, u32)>) {
        self.fuel_item = item;
    }

    #[must_use]
    pub fn fuel_charges(&self) -> i32 {
        self.fuel_charges
    }

    #[must_use]
    pub fn brew_progress(&self) -> i32 {
        self.brew_time
    }

    #[must_use]
    pub fn is_brewing(&self) -> bool {
        self.brew_time > 0
    }

    /// The real is-brewable rule: the ingredient slot must hold a valid
    /// ingredient, and at least one bottle slot must actually respond to it.
    fn is_brewable(&self) -> bool {
        let Some((ingredient, _)) = &self.ingredient else {
            return false;
        };
        if !is_ingredient(ingredient) {
            return false;
        }
        self.bottles
            .iter()
            .flatten()
            .any(|b| has_mix(b, ingredient))
    }

    fn do_brew(&mut self) {
        let Some((ingredient, _)) = self.ingredient.clone() else {
            return;
        };
        for slot in &mut self.bottles {
            if let Some(bottle) = slot {
                *bottle = mix_bottle(bottle, &ingredient);
            }
        }
        if let Some((_, count)) = self.ingredient.as_mut() {
            *count -= 1;
            if *count == 0 {
                self.ingredient = None;
            }
        }
    }

    /// Advances by exactly one server tick — a direct port of
    /// the real per-tick rule; see the module doc
    /// comment for the control flow this mirrors step-by-step.
    pub fn tick(&mut self) -> BrewTick {
        let mut out = BrewTick::default();

        if self.fuel_charges <= 0 {
            let refill = matches!(&self.fuel_item, Some((item, _)) if item == "minecraft:blaze_powder");
            if refill {
                self.fuel_charges = FUEL_USES;
                if let Some((_, count)) = self.fuel_item.as_mut() {
                    *count -= 1;
                    if *count == 0 {
                        self.fuel_item = None;
                    }
                }
                out.fuel_refilled = true;
            }
        }

        let brewable = self.is_brewable();
        let is_brewing = self.brew_time > 0;
        if is_brewing {
            self.brew_time -= 1;
            let done = self.brew_time == 0;
            let ingredient_swapped = self.locked_ingredient
                != self.ingredient.as_ref().map(|(i, _)| i.clone());
            if done && brewable {
                self.do_brew();
                self.locked_ingredient = None;
                out.brewed = true;
            } else if !brewable || ingredient_swapped {
                self.brew_time = 0;
                self.locked_ingredient = None;
            }
        } else if brewable && self.fuel_charges > 0 {
            self.fuel_charges -= 1;
            self.brew_time = BREW_TIME_TICKS;
            self.locked_ingredient = self.ingredient.as_ref().map(|(i, _)| i.clone());
            out.started = true;
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn water() -> Bottle {
        Bottle::new(BottleKind::Potion, "minecraft:water")
    }

    #[test]
    fn fresh_stand_is_idle_and_empty() {
        let s = BrewingStand::new();
        assert!(!s.is_brewing());
        assert_eq!(s.fuel_charges(), 0);
        assert!(s.bottle(0).is_none());
    }

    #[test]
    fn nether_wart_turns_water_into_awkward() {
        let bottle = mix_bottle(&water(), "minecraft:nether_wart");
        assert_eq!(bottle, Bottle::new(BottleKind::Potion, "minecraft:awkward"));
    }

    #[test]
    fn gunpowder_promotes_potion_to_splash_not_the_potion_type() {
        let awkward = Bottle::new(BottleKind::Potion, "minecraft:awkward");
        let splashed = mix_bottle(&awkward, "minecraft:gunpowder");
        assert_eq!(splashed, Bottle::new(BottleKind::Splash, "minecraft:awkward"));
    }

    #[test]
    fn dragon_breath_promotes_splash_to_lingering() {
        let splash = Bottle::new(BottleKind::Splash, "minecraft:swiftness");
        let lingering = mix_bottle(&splash, "minecraft:dragon_breath");
        assert_eq!(lingering, Bottle::new(BottleKind::Lingering, "minecraft:swiftness"));
    }

    /// **Control**: dragon's breath must not promote a plain `Potion`
    /// bottle directly to `Lingering` — it only promotes an already-`Splash`
    /// bottle, matching vanilla's two-step chain rather than a shortcut.
    #[test]
    fn dragon_breath_does_not_promote_a_plain_potion_bottle() {
        let potion = Bottle::new(BottleKind::Potion, "minecraft:swiftness");
        let unchanged = mix_bottle(&potion, "minecraft:dragon_breath");
        assert_eq!(unchanged, potion, "no mix rule applies; bottle must pass through unchanged");
    }

    #[test]
    fn unrelated_ingredient_leaves_bottle_unchanged() {
        let bottle = water();
        let unchanged = mix_bottle(&bottle, "minecraft:diamond");
        assert_eq!(unchanged, bottle);
    }

    #[test]
    fn is_ingredient_covers_both_tables_and_rejects_junk() {
        assert!(is_ingredient("minecraft:nether_wart"));
        assert!(is_ingredient("minecraft:gunpowder"));
        assert!(is_ingredient("minecraft:dragon_breath"));
        assert!(!is_ingredient("minecraft:diamond"));
    }

    /// The magnitude check: fuel refills to exactly [`FUEL_USES`] (20), one
    /// blaze powder charges exactly 20 brews (the charge count decrements
    /// once per brew *started*, not per tick).
    #[test]
    fn one_blaze_powder_charges_exactly_twenty_brews() {
        let mut s = BrewingStand::new();
        s.set_fuel_item(Some(("minecraft:blaze_powder".into(), 1)));
        s.set_bottle(0, Some(water()));
        s.set_ingredient(Some(("minecraft:nether_wart".into(), 64)));

        for brew_number in 1..=20 {
            let refill_tick = s.tick();
            if brew_number == 1 {
                assert!(refill_tick.fuel_refilled, "fuel must refill before the first brew");
                assert_eq!(s.fuel_item(), None, "the single blaze powder must be fully consumed");
            }
            assert!(refill_tick.started, "expected brew {brew_number} to start");
            assert_eq!(s.fuel_charges(), FUEL_USES - brew_number);

            for t in 1..BREW_TIME_TICKS {
                let tick = s.tick();
                assert!(!tick.brewed, "brewed early at brew {brew_number} tick {t}");
            }
            let tick = s.tick();
            assert!(tick.brewed, "expected brew {brew_number} to complete at exactly tick {BREW_TIME_TICKS}");
            assert_eq!(s.bottle(0), Some(&Bottle::new(BottleKind::Potion, "minecraft:awkward")));
            // Reset the bottle back to water for the next iteration so each
            // of the 20 brews exercises the same water->awkward transition.
            s.set_bottle(0, Some(water()));
        }

        // The 21st attempt must not start — no fuel charges and no fuel
        // item left to refill from.
        assert_eq!(s.fuel_charges(), 0);
        let tick = s.tick();
        assert!(!tick.started, "must not start a 21st brew with no fuel left");
    }

    #[test]
    fn brew_completes_at_exactly_tick_400_and_consumes_one_ingredient() {
        let mut s = BrewingStand::new();
        s.set_fuel_item(Some(("minecraft:blaze_powder".into(), 1)));
        s.set_bottle(0, Some(water()));
        s.set_ingredient(Some(("minecraft:nether_wart".into(), 3)));

        // The tick that *starts* a brew (setting `brew_time = 400`) does
        // not also decrement it — the real per-tick rule's is-brewing/start
        // branch is mutually exclusive (see the module doc comment's
        // control-flow restatement). So completion is exactly `BREW_TIME_TICKS`
        // *additional* tick calls after the start call, not
        // `BREW_TIME_TICKS` calls total.
        assert!(s.tick().started); // start call
        for t in 1..BREW_TIME_TICKS {
            let tick = s.tick();
            assert!(!tick.brewed, "brewed early at {t} ticks after start");
        }
        let tick = s.tick();
        assert!(tick.brewed, "expected completion at exactly {BREW_TIME_TICKS} ticks after start");
        assert_eq!(s.bottle(0), Some(&Bottle::new(BottleKind::Potion, "minecraft:awkward")));
        assert_eq!(s.ingredient(), Some(("minecraft:nether_wart", 2)));
    }

    /// **Control**: swapping the ingredient mid-brew aborts it (brew time
    /// reset to 0), even with the vast majority of the 400 ticks already
    /// elapsed — proves the lock is real, not merely never exercised.
    #[test]
    fn swapping_ingredient_mid_brew_aborts_it() {
        let mut s = BrewingStand::new();
        s.set_fuel_item(Some(("minecraft:blaze_powder".into(), 1)));
        s.set_bottle(0, Some(water()));
        s.set_ingredient(Some(("minecraft:nether_wart".into(), 1)));
        s.tick(); // starts the brew, locks in nether_wart

        for _ in 0..390 {
            s.tick();
        }
        assert!(s.is_brewing(), "should still be brewing with 10 ticks left");

        // Swap to a different (but still valid) ingredient.
        s.set_ingredient(Some(("minecraft:redstone".into(), 1)));
        let tick = s.tick();
        assert!(!tick.brewed);
        assert!(!s.is_brewing(), "the swap must abort the brew, not let it finish on redstone");
        assert_eq!(s.bottle(0), Some(&water()), "bottle must be untouched by the aborted brew");
    }

    /// **Control**: with no bottle in any slot at all, the real is-brewable
    /// rule is false (nothing for `.any(has_mix)` to find), so a brew must never
    /// start — even with fuel and a recognized ingredient present.
    #[test]
    fn no_bottle_at_all_means_no_brew_starts() {
        let mut s = BrewingStand::new();
        s.set_fuel_item(Some(("minecraft:blaze_powder".into(), 1)));
        s.set_ingredient(Some(("minecraft:nether_wart".into(), 1)));

        for t in 0..50 {
            let tick = s.tick();
            assert!(!tick.started, "unexpected brew start at tick {t}");
        }
        assert!(!s.is_brewing());
    }

    /// Potion-type mixes (unlike container promotions) key only on the
    /// potion value, not the bottle's container kind — nether wart turns an
    /// already-*splash* water bottle into a splash awkward potion exactly
    /// as readily as a plain one, because the real mix rule's
    /// potion-type-table loop checks only the potion value, with no
    /// container-kind test (that check belongs to the container-promotion table
    /// alone).
    /// A control worth having on record precisely because it is easy to
    /// mis-assume the opposite.
    #[test]
    fn potion_mixes_ignore_container_kind() {
        let splash_water = Bottle::new(BottleKind::Splash, "minecraft:water");
        let mixed = mix_bottle(&splash_water, "minecraft:nether_wart");
        assert_eq!(mixed, Bottle::new(BottleKind::Splash, "minecraft:awkward"));
    }

    /// **Control**: an unrecognized ingredient item never starts a brew,
    /// even with a perfectly good bottle and fuel available.
    #[test]
    fn unrecognized_ingredient_never_starts_a_brew() {
        let mut s = BrewingStand::new();
        s.set_fuel_item(Some(("minecraft:blaze_powder".into(), 1)));
        s.set_bottle(0, Some(water()));
        s.set_ingredient(Some(("minecraft:diamond".into(), 1)));

        let tick = s.tick();
        assert!(!tick.started);
        assert!(!s.is_brewing());
    }

    #[test]
    fn splash_and_lingering_are_the_same_400_tick_cycle_as_any_ingredient() {
        // Verifies the "not a separate post-processing step" claim in the
        // module doc comment directly: gunpowder brews on the identical
        // BREW_TIME_TICKS schedule as nether wart.
        let mut s = BrewingStand::new();
        s.set_fuel_item(Some(("minecraft:blaze_powder".into(), 1)));
        s.set_bottle(0, Some(Bottle::new(BottleKind::Potion, "minecraft:awkward")));
        s.set_ingredient(Some(("minecraft:gunpowder".into(), 1)));

        assert!(s.tick().started); // start call; see the previous test's
        // comment for why completion is BREW_TIME_TICKS calls *after* this.
        for _ in 1..BREW_TIME_TICKS {
            s.tick();
        }
        let tick = s.tick();
        assert!(tick.brewed, "expected the splash promotion to complete at exactly {BREW_TIME_TICKS} ticks after start");
        assert_eq!(
            s.bottle(0),
            Some(&Bottle::new(BottleKind::Splash, "minecraft:awkward")),
            "container promoted, potion type untouched"
        );
    }
}
