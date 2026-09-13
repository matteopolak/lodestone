//! The server's own registry orders, by holder id.
//!
//! ## What it is
//!
//! Name tables for the synchronized registries whose **order is the server's, not
//! ours**. A holder id is only meaningful against the order the server sent, and
//! resolving one through a table we derived ourselves is wrong in the worst
//! possible way: silently, with a valid-looking id that names the wrong thing.
//!
//! ## How it works
//!
//! One `Vec<String>` per registry, indexed by holder id, folded from the
//! `*RegistryNames` events the adapter emits at `Login`. Absent means *fall back*,
//! never "the registry is empty" — a server that sent no `registry_data` for a
//! registry is a server we must degrade against, and
//! [`RegistryOrder::enchantments`] returning an empty slice is how a caller tells
//! the two apart.
//!
//! ## The bug this exists to delete
//!
//! `Sim::riptide_level` resolved `minecraft:riptide` through a hardcoded holder id
//! of **32**, because `riptide` is the 33rd of 26.2's 43 built-in enchantments in
//! resource-location-sorted order. Correct against vanilla 26.2; wrong against any
//! data pack that adds, removes or reorders an enchantment sorting before it —
//! and wrong without any error, since the id still resolves to *an* enchantment.
//! The exact shape of the mesher's `FALLBACK_BIOME_NAMES` bug, one registry over,
//! and the table was already decoded the whole time: `ClientRegistries::entry_names`
//! has carried it for a long time and simply never left the version crate.
//!
//! ## How to change it
//!
//! To add a registry, add a field, an accessor, and an arm to
//! [`RegistryOrder::apply`] — and emit the matching `*RegistryNames` event from
//! every adapter that can serve that registry, or the field stays empty on those
//! families and every caller silently takes the fallback path.
//!
//! ## Dependencies
//!
//! [`lodestone_model::event::ClientEvent`] only.

use lodestone_model::event::ClientEvent;

/// Registry name tables in the server's own holder-id order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryOrder {
    enchantments: Vec<String>,
}

impl RegistryOrder {
    /// An empty table set — the pre-`Login` state, and the state a caller must
    /// read as "use your fallback".
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The `minecraft:enchantment` registry in holder-id order, or an empty slice
    /// when the server sent none.
    #[must_use]
    pub fn enchantments(&self) -> &[String] {
        &self.enchantments
    }

    /// The holder id of `name` in the `minecraft:enchantment` registry.
    ///
    /// `None` means either "this server has no such enchantment" or "we have no
    /// table yet"; check [`Self::enchantments`] for emptiness to tell them apart.
    /// A caller that treats `None` as "level 0" is right either way — an absent
    /// enchantment cannot be on a stack — but a caller that falls back to a
    /// hardcoded id must know which case it is in, which is the whole point.
    #[must_use]
    pub fn enchantment_id(&self, name: &str) -> Option<i32> {
        self.enchantments
            .iter()
            .position(|entry| entry == name)
            .and_then(|index| i32::try_from(index).ok())
    }

    /// Folds one event, returning whether it belonged to this store.
    pub fn apply(&mut self, event: &ClientEvent) -> bool {
        match event {
            ClientEvent::EnchantmentRegistryNames { names } => {
                // A whole-table replace, not a merge: a re-entry into
                // Configuration resends the registries and is followed by a fresh
                // `Login`, so merging would leave entries from the old generation
                // at ids the new one reuses.
                self.enchantments = names.clone();
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::RegistryOrder;
    use lodestone_model::event::ClientEvent;

    /// The expectation is that a *reordered* registry resolves differently from
    /// vanilla's — which is the entire reason the hardcoded id was a bug. Asserting
    /// only vanilla's own order would pass for a table that ignored the wire.
    #[test]
    fn an_id_comes_from_the_servers_order_not_ours() {
        let mut order = RegistryOrder::new();
        order.apply(&ClientEvent::EnchantmentRegistryNames {
            names: vec![
                "minecraft:riptide".to_owned(),
                "minecraft:sharpness".to_owned(),
            ],
        });
        assert_eq!(
            order.enchantment_id("minecraft:riptide"),
            Some(0),
            "riptide is holder 0 on this server, not the vanilla 32"
        );
        assert_eq!(order.enchantment_id("minecraft:sharpness"), Some(1));
        assert_eq!(order.enchantment_id("minecraft:nonesuch"), None);
    }

    /// A second generation must replace, not append — ids are reused.
    #[test]
    fn a_second_registry_generation_replaces_the_table() {
        let mut order = RegistryOrder::new();
        order.apply(&ClientEvent::EnchantmentRegistryNames {
            names: vec!["a".to_owned(), "b".to_owned()],
        });
        order.apply(&ClientEvent::EnchantmentRegistryNames {
            names: vec!["c".to_owned()],
        });
        assert_eq!(order.enchantments(), ["c".to_owned()]);
    }

    /// Empty is "fall back", and a caller must be able to see that it is empty.
    #[test]
    fn an_empty_table_is_distinguishable_from_a_missing_entry() {
        let order = RegistryOrder::new();
        assert!(order.enchantments().is_empty());
        assert_eq!(order.enchantment_id("minecraft:riptide"), None);
    }

    #[test]
    fn an_unrelated_event_is_rejected() {
        let mut order = RegistryOrder::new();
        assert!(!order.apply(&ClientEvent::KeepAlive { id: 1 }));
    }
}
