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
        } else {
            self.order.push(effect.id.clone());
            self.effects.push(effect);
        }
    }

    /// Removes an effect by id, returning it if present.
    pub fn remove(&mut self, id: &Identifier) -> Option<StatusEffect> {
        let i = self.index_of(id)?;
        self.order.remove(i);
        Some(self.effects.remove(i))
    }

    /// Looks up an active effect.
    #[must_use]
    pub fn get(&self, id: &Identifier) -> Option<&StatusEffect> {
        self.index_of(id).map(|i| &self.effects[i])
    }

    /// Clears all effects (e.g. on death/respawn or milk).
    pub fn clear(&mut self) {
        self.order.clear();
        self.effects.clear();
    }

    /// Advances all finite effects by `ticks`, dropping any that expire. Kept
    /// explicit rather than automatic so the caller controls the game clock.
    /// Infinite effects (`duration_ticks < 0`) are never decremented or removed.
    pub fn tick(&mut self, ticks: i32) {
        let mut i = 0;
        while i < self.effects.len() {
            let effect = &mut self.effects[i];
            if effect.duration_ticks >= 0 {
                effect.duration_ticks -= ticks;
                if effect.duration_ticks <= 0 {
                    self.order.remove(i);
                    self.effects.remove(i);
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
}
