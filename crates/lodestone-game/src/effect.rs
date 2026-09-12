//! Active status effects (potion effects) shown on the HUD.
//!
//! Version-free canonical state a protocol adapter drives from `update_mob_effect`
//! (add/replace) and `remove_mob_effect`. The effect *identity* is a canonical
//! [`Identifier`] (e.g. `minecraft:speed`), never a version-specific numeric id;
//! an adapter for an older protocol translates its numeric ids upward.

use lodestone_model::Identifier;

/// A single active status effect on the player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusEffect {
    /// Canonical effect id, e.g. `minecraft:speed`.
    pub id: Identifier,
    /// Amplifier; the displayed level is `amplifier + 1`.
    pub amplifier: u8,
    /// Remaining duration in ticks; `-1` means infinite.
    pub duration_ticks: i32,
    /// Whether the effect came from a beacon/ambient source (fainter icon).
    pub ambient: bool,
    /// Whether to emit potion particles.
    pub show_particles: bool,
    /// Whether to show the HUD icon.
    pub show_icon: bool,
    /// Whether a client should animate an effect-specific visual transition.
    ///
    /// A false value requests an immediate visual state. The wire flag does
    /// not control normal particle visibility; [`Self::show_particles`] does.
    pub blend: bool,
}

/// The typed effect subset consumed by block-breaking prediction.
///
/// The wire/read-model boundary keeps canonical identifiers so unknown and
/// future effects remain representable. Once inside this gameplay consumer,
/// only these three roles matter: the two possible Haste sources are folded to
/// their strongest amplifier and Mining Fatigue remains independent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DigSpeedEffects {
    /// Strongest active Haste or Conduit Power amplifier, if any.
    pub haste_amplifier: Option<u32>,
    /// Active Mining Fatigue amplifier, if any.
    pub mining_fatigue: Option<u32>,
}

impl StatusEffect {
    /// A visible, non-ambient effect with the given remaining duration.
    #[must_use]
    pub fn new(id: Identifier, amplifier: u8, duration_ticks: i32) -> Self {
        Self {
            id,
            amplifier,
            duration_ticks,
            ambient: false,
            show_particles: true,
            show_icon: true,
            blend: true,
        }
    }

    /// Whether this effect never expires.
    #[must_use]
    pub fn is_infinite(&self) -> bool {
        self.duration_ticks < 0
    }

    /// The level to display (`amplifier + 1`).
    #[must_use]
    pub fn level(&self) -> u32 {
        u32::from(self.amplifier) + 1
    }
}

/// The player's active effects, preserving insertion order for a stable HUD
/// row layout (vanilla groups beneficial/harmful, but order stability is what a
/// read-model owes the renderer).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActiveEffects {
    order: Vec<Identifier>,
    effects: Vec<StatusEffect>,
    elapsed_ticks: Vec<i32>,
}

impl ActiveEffects {
    /// A new empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn index_of(&self, id: &Identifier) -> Option<usize> {
        self.order.iter().position(|k| k == id)
    }

    /// Adds or replaces an effect (an `update_mob_effect` packet). Replacing
    /// keeps the original row position, matching vanilla behaviour.
    pub fn apply(&mut self, effect: StatusEffect) {
        if let Some(i) = self.index_of(&effect.id) {
            self.effects[i] = effect;
            self.elapsed_ticks[i] = 0;
        } else {
            self.order.push(effect.id.clone());
            self.effects.push(effect);
            self.elapsed_ticks.push(0);
        }
    }

    /// Removes an effect by id, returning it if present.
    pub fn remove(&mut self, id: &Identifier) -> Option<StatusEffect> {
        let i = self.index_of(id)?;
        self.order.remove(i);
        self.elapsed_ticks.remove(i);
        Some(self.effects.remove(i))
    }

    /// Looks up an active effect.
    #[must_use]
    pub fn get(&self, id: &Identifier) -> Option<&StatusEffect> {
        self.index_of(id).map(|i| &self.effects[i])
    }

    /// Ticks elapsed since this effect was last applied or refreshed.
    #[must_use]
    pub fn elapsed_ticks(&self, id: &Identifier) -> Option<i32> {
        self.index_of(id).map(|i| self.elapsed_ticks[i])
    }

    /// Clears all effects (e.g. on death/respawn or milk).
    pub fn clear(&mut self) {
        self.order.clear();
        self.effects.clear();
        self.elapsed_ticks.clear();
    }

    /// Advances all finite effects by `ticks`, dropping any that expire. Kept
    /// explicit rather than automatic so the caller controls the game clock.
    /// Infinite effects (`duration_ticks < 0`) are never decremented or removed.
    pub fn tick(&mut self, ticks: i32) {
        let mut i = 0;
        while i < self.effects.len() {
            let effect = &mut self.effects[i];
            self.elapsed_ticks[i] = self.elapsed_ticks[i].saturating_add(ticks);
            if effect.duration_ticks >= 0 {
                effect.duration_ticks -= ticks;
                if effect.duration_ticks <= 0 {
                    self.order.remove(i);
                    self.effects.remove(i);
                    self.elapsed_ticks.remove(i);
                    continue;
                }
            }
            i += 1;
        }
    }

    /// Active effects in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &StatusEffect> {
        self.effects.iter()
    }

    /// Resolves the effect roles used by the digging-speed rule once at the
    /// consumer boundary. Unrelated effects are deliberately ignored.
    #[must_use]
    pub fn dig_speed_effects(&self) -> DigSpeedEffects {
        let mut result = DigSpeedEffects::default();
        for effect in &self.effects {
            match (effect.id.namespace(), effect.id.path()) {
                ("minecraft", "haste") | ("minecraft", "conduit_power") => {
                    let amplifier = u32::from(effect.amplifier);
                    result.haste_amplifier = result.haste_amplifier.max(Some(amplifier));
                }
                ("minecraft", "mining_fatigue") => {
                    result.mining_fatigue = Some(u32::from(effect.amplifier));
                }
                _ => {}
            }
        }
        result
    }

    /// Number of active effects.
    #[must_use]
    pub fn len(&self) -> usize {
        self.effects.len()
    }

    /// Whether no effects are active.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.effects.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(s: &str) -> Identifier {
        s.parse().unwrap()
    }

    #[test]
    fn apply_replaces_in_place() {
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect::new(id("minecraft:speed"), 0, 200));
        fx.apply(StatusEffect::new(id("minecraft:haste"), 1, 100));
        fx.apply(StatusEffect::new(id("minecraft:speed"), 2, 400));
        // Replacement keeps position and updates fields.
        let ids: Vec<_> = fx.iter().map(|e| e.id.to_string()).collect();
        assert_eq!(ids, ["minecraft:speed", "minecraft:haste"]);
        assert_eq!(fx.get(&id("minecraft:speed")).unwrap().amplifier, 2);
        assert_eq!(fx.get(&id("minecraft:speed")).unwrap().level(), 3);
    }

    #[test]
    fn tick_expires_finite_keeps_infinite() {
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect::new(id("minecraft:speed"), 0, 40));
        fx.apply(StatusEffect::new(id("minecraft:night_vision"), 0, -1));
        fx.tick(40);
        assert!(fx.get(&id("minecraft:speed")).is_none(), "finite expired");
        assert!(
            fx.get(&id("minecraft:night_vision")).unwrap().is_infinite(),
            "infinite retained"
        );
        assert_eq!(fx.len(), 1);
    }

    #[test]
    fn remove_and_clear() {
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect::new(id("minecraft:speed"), 0, 40));
        assert!(fx.remove(&id("minecraft:speed")).is_some());
        assert!(fx.is_empty());
        fx.apply(StatusEffect::new(id("minecraft:haste"), 0, 40));
        fx.clear();
        assert!(fx.is_empty());
    }

    #[test]
    fn dig_speed_effects_ignore_unrelated_status_effects() {
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect::new(id("minecraft:speed"), 4, 200));
        assert_eq!(fx.dig_speed_effects(), DigSpeedEffects::default());

        fx.apply(StatusEffect::new(id("minecraft:haste"), 1, 200));
        fx.apply(StatusEffect::new(id("minecraft:conduit_power"), 3, 200));
        fx.apply(StatusEffect::new(id("minecraft:mining_fatigue"), 0, 200));
        assert_eq!(
            fx.dig_speed_effects(),
            DigSpeedEffects {
                haste_amplifier: Some(3),
                mining_fatigue: Some(0),
            }
        );
    }

    #[test]
    fn elapsed_ticks_advance_and_reset_when_an_effect_is_refreshed() {
        let speed = id("minecraft:speed");
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect::new(speed.clone(), 0, 200));
        fx.tick(12);
        assert_eq!(fx.elapsed_ticks(&speed), Some(12));

        fx.apply(StatusEffect::new(speed.clone(), 0, 200));
        assert_eq!(fx.elapsed_ticks(&speed), Some(0));
        assert!(fx.remove(&speed).is_some());
        assert_eq!(fx.elapsed_ticks(&speed), None);
    }
}
