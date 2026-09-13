//! Server-authoritative item-use cooldowns.
//!
//! ## What it is
//!
//! [`ItemCooldowns`] retains each server-announced cooldown group's remaining
//! lifetime. The HUD reads its fraction for the dark veil over matching hotbar
//! items.
//!
//! ## How it works
//!
//! `ClientEvent::ItemCooldown` replaces the group lifetime, including a zero
//! duration which clears it. [`tick`](ItemCooldowns::tick) advances every live
//! group once per game tick; [`fraction_for`](ItemCooldowns::fraction_for) is a
//! read-only projection, so drawing cannot age state.
//!
//! ## How to change it
//!
//! A group currently matches an item's identifier directly. Keep that fallback
//! when decoded per-stack group overrides arrive: an absent override means the
//! item identifier is the group. Do not move the clock into a renderer; bots and
//! the game shell must agree about expiry.
//!
//! ## Configuration
//!
//! Cooldown duration is supplied by the server in ticks. There are no local
//! settings or environment variables.
//!
//! ## Dependencies
//!
//! Depends only on `lodestone-model` for the version-free event and identifier.

use std::collections::BTreeMap;

use lodestone_model::{ClientEvent, Identifier};

/// Remaining server item-use cooldowns, keyed by their wire cooldown group.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ItemCooldowns {
    groups: BTreeMap<Identifier, Cooldown>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cooldown {
    remaining_ticks: u32,
    total_ticks: u32,
}

impl ItemCooldowns {
    /// Folds an item-cooldown event, returning whether it belonged here.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        let ClientEvent::ItemCooldown {
            group,
            duration_ticks,
        } = event
        else {
            return false;
        };
        let duration = u32::try_from(*duration_ticks).unwrap_or(0);
        if duration == 0 {
            self.groups.remove(group);
        } else {
            self.groups.insert(
                group.clone(),
                Cooldown {
                    remaining_ticks: duration,
                    total_ticks: duration,
                },
            );
        }
        true
    }

    /// Advances all groups by one game tick, dropping exactly-expired entries.
    pub fn tick(&mut self) {
        self.groups.retain(|_, cooldown| {
            cooldown.remaining_ticks = cooldown.remaining_ticks.saturating_sub(1);
            cooldown.remaining_ticks != 0
        });
    }

    /// Remaining fraction for an item whose default cooldown group is its id.
    #[must_use]
    pub fn fraction_for(&self, item: &Identifier) -> f32 {
        let Some(cooldown) = self.groups.get(item) else {
            return 0.0;
        };
        cooldown.remaining_ticks as f32 / cooldown.total_ticks as f32
    }
}

#[cfg(test)]
mod tests {
    use super::ItemCooldowns;
    use lodestone_model::{ClientEvent, Identifier};

    fn key(value: &str) -> Identifier {
        value.parse().expect("valid identifier")
    }

    #[test]
    fn cooldown_folds_then_expires_at_the_reported_tick() {
        let pearl = key("minecraft:ender_pearl");
        let mut cooldowns = ItemCooldowns::default();
        assert!(cooldowns.apply(&ClientEvent::ItemCooldown {
            group: pearl.clone(),
            duration_ticks: 4,
        }));
        assert_eq!(cooldowns.fraction_for(&pearl), 1.0);
        cooldowns.tick();
        assert_eq!(cooldowns.fraction_for(&pearl), 0.75);
        for _ in 0..3 {
            cooldowns.tick();
        }
        assert_eq!(cooldowns.fraction_for(&pearl), 0.0);
    }

    #[test]
    fn unrelated_item_and_zero_duration_are_negative_controls() {
        let pearl = key("minecraft:ender_pearl");
        let chorus = key("minecraft:chorus_fruit");
        let mut cooldowns = ItemCooldowns::default();
        assert!(cooldowns.apply(&ClientEvent::ItemCooldown {
            group: pearl.clone(),
            duration_ticks: 8,
        }));
        assert_eq!(cooldowns.fraction_for(&chorus), 0.0);
        assert!(cooldowns.apply(&ClientEvent::ItemCooldown {
            group: pearl.clone(),
            duration_ticks: 0,
        }));
        assert_eq!(cooldowns.fraction_for(&pearl), 0.0);
    }
}
