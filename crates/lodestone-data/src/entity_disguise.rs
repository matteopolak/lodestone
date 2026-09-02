//! Plugin-defined entity types, and the vanilla type each one **streams as**.
//!
//! # What this is
//!
//! A registry mapping a plugin's own entity kind (`myplugin:sentry`) to a vanilla
//! entity type it is disguised as on the wire (`minecraft:armor_stand`). It lives
//! in `lodestone-data` rather than in `lodestone-game` or `lodestone-ecs` for one
//! reason: it is the **only** place both the client and `lodestone-server` can
//! reach. `lodestone-server` deliberately does not depend on `lodestone-ecs` or
//! `lodestone-game` (its `Cargo.toml` says so and gives the two costs), and the
//! server is where a spawn actually needs a type id.
//!
//! # Why disguises, and not a new wire type
//!
//! `add_entity` carries the entity type as a network **registry index** into a
//! 158-entry table. There is no room in the protocol for a novel type, and vanilla
//! itself has no such mechanism — which is why real Paper servers implement custom
//! mobs as a vanilla entity with custom NBT, a custom name and custom AI, not as a
//! new registry entry. So a custom
//! entity kind is a *logical* identity on the server plus a vanilla type on the
//! wire, and this module is the mapping between them.
//!
//! # The trap this exists to close, stated precisely
//!
//! `crates/versions/26.2/src/server_protocol.rs`'s `encode_add_entity_body` does:
//!
//! ```text
//! let type_id = entity_type_id(&entity.entity_type.to_string()).unwrap_or(0);
//! ```
//!
//! **Network type id `0` is `minecraft:acacia_boat`.** So an entity type the table
//! does not know — a typo, or exactly the plugin-namespaced key this issue is
//! about — streams as an acacia boat, with **no error, no warning and no log line
//! anywhere**. The client renders a boat, the server thinks it spawned a sentry,
//! and nothing in either process reports a problem.
//!
//! The fix is not to make the fallback smarter. It is to make the failure happen
//! at **registration** time instead of at stream time:
//!
//! * [`EntityDisguises::register`] resolves the vanilla target to a wire id
//!   *immediately* and returns [`EntityDisguiseError::UnknownVanillaType`] if it
//!   cannot. A disguise that would have streamed as a boat cannot be registered.
//! * [`EntityDisguises::resolve_wire_id`] is the safe replacement for that
//!   `unwrap_or(0)` line: it returns `Option<i32>` and **never** substitutes a
//!   default. A caller must decide what to do with `None`, and the honest choice
//!   is to refuse the spawn rather than stream the wrong entity.
//!
//! # Usage
//!
//! ```
//! use lodestone_data::entity_disguise::EntityDisguises;
//!
//! let mut disguises = EntityDisguises::new();
//! disguises
//!     .register("myplugin:sentry", "minecraft:armor_stand")
//!     .expect("armor_stand is a real vanilla type");
//!
//! // A custom kind resolves to the *disguise's* wire id.
//! let armor_stand = lodestone_data::entity_types::entity_type_id("minecraft:armor_stand");
//! assert_eq!(disguises.resolve_wire_id("myplugin:sentry"), armor_stand);
//!
//! // A vanilla type still resolves to itself.
//! assert_eq!(disguises.resolve_wire_id("minecraft:pig"),
//!            lodestone_data::entity_types::entity_type_id("minecraft:pig"));
//!
//! // And an unregistered custom kind is a definite miss -- NOT id 0.
//! assert_eq!(disguises.resolve_wire_id("myplugin:unregistered"), None);
//! ```
//!
//! # How to change it
//!
//! * **Never add a fallback to `resolve_wire_id`.** Its whole value is that it has
//!   none. If a caller needs a default, that caller states it, in a place where a
//!   reader can see the decision.
//! * **Entity metadata indices are not hand-countable.** A disguise that also
//!   wants to send metadata (an armour stand's own client-flags field, a creeper's
//!   swell) must take its index from `EntityDataIndexOracle.java`'s dump, not from
//!   counting. Index 15 is the mob class's flags **and** the armour stand's own
//!   client-flags field,
//!   and index 8 is the living-entity class's own flags field **and**
//!   the arrow base class's own flags field — which guard separates the real claimants depends
//!   on which classes collide, so the census column has to be chosen per
//!   collision. `CLAUDE.md` records both instances.
//! * The lookup is a `BTreeMap` keyed by the joined `namespace:path` string, to
//!   match `entity_types`' own table representation and avoid a `ResourceKey`
//!   dependency in the hot path.
//!
//! # Dependencies
//!
//! [`crate::entity_types`] only. No protocol crate, no ECS.

use alloc_shim::{BTreeMap, String, ToString};

use crate::entity_types::entity_type_id;

/// Small shim so this module reads the same whether or not the crate is `std`.
/// `lodestone-data` is `std` today; this exists to keep the imports in one place.
mod alloc_shim {
    pub use std::collections::BTreeMap;
    pub use std::string::{String, ToString};
}

/// The namespace the vanilla entity registry owns.
pub const VANILLA_NAMESPACE: &str = "minecraft";

/// One registered disguise: a plugin's kind, the vanilla type it streams as, and
/// that type's wire id resolved **at registration time**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntityDisguise {
    custom: String,
    streams_as: String,
    wire_id: i32,
}

impl EntityDisguise {
    /// The plugin's own entity kind.
    #[must_use]
    pub fn custom(&self) -> &str {
        &self.custom
    }

    /// The vanilla entity type this streams as.
    #[must_use]
    pub fn streams_as(&self) -> &str {
        &self.streams_as
    }

    /// The network entity-type id this streams as.
    ///
    /// Resolved when the disguise was registered, which is what makes it
    /// impossible for a registered disguise to stream as the wrong thing later.
    #[must_use]
    pub fn wire_id(&self) -> i32 {
        self.wire_id
    }
}

/// Every registered plugin entity kind.
#[derive(Debug, Clone, Default)]
pub struct EntityDisguises {
    by_custom: BTreeMap<String, EntityDisguise>,
}

impl EntityDisguises {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `custom` as a disguise for the vanilla type `streams_as`.
    ///
    /// # Errors
    ///
    /// * [`EntityDisguiseError::ReservedNamespace`] — `custom` is `minecraft:`-
    ///   namespaced. It would shadow a real vanilla type, and every render-side
    ///   lookup keyed off the vanilla registry would then disagree with this one.
    /// * [`EntityDisguiseError::UnknownVanillaType`] — `streams_as` is not in the
    ///   158-entry table. **This is the check that matters**: without it the
    ///   disguise would register happily and stream as `minecraft:acacia_boat`
    ///   forever, silently.
    /// * [`EntityDisguiseError::Duplicate`] — `custom` is already registered.
    pub fn register(
        &mut self,
        custom: &str,
        streams_as: &str,
    ) -> Result<&EntityDisguise, EntityDisguiseError> {
        if namespace_of(custom) == VANILLA_NAMESPACE {
            return Err(EntityDisguiseError::ReservedNamespace(custom.to_string()));
        }
        if self.by_custom.contains_key(custom) {
            return Err(EntityDisguiseError::Duplicate(custom.to_string()));
        }
        // Resolved now, not at stream time. An unresolvable target is refused
        // here rather than becoming an acacia boat on the wire later.
        let Some(wire_id) = entity_type_id(streams_as) else {
            return Err(EntityDisguiseError::UnknownVanillaType {
                custom: custom.to_string(),
                streams_as: streams_as.to_string(),
            });
        };
        let entry = EntityDisguise {
            custom: custom.to_string(),
            streams_as: streams_as.to_string(),
            wire_id,
        };
        Ok(self
            .by_custom
            .entry(custom.to_string())
            .insert_entry(entry)
            .into_mut())
    }

    /// Removes a disguise, returning it.
    pub fn unregister(&mut self, custom: &str) -> Option<EntityDisguise> {
        self.by_custom.remove(custom)
    }

    /// The disguise registered for `custom`, if any.
    #[must_use]
    pub fn get(&self, custom: &str) -> Option<&EntityDisguise> {
        self.by_custom.get(custom)
    }

    /// **The safe replacement for `entity_type_id(name).unwrap_or(0)`.**
    ///
    /// Resolution order:
    ///
    /// 1. a real vanilla type resolves to its own id;
    /// 2. a registered custom kind resolves to its disguise's id;
    /// 3. anything else is `None`.
    ///
    /// There is deliberately **no** fallback. `None` means "this entity must not
    /// be streamed", and a caller that substitutes `0` has reintroduced the bug
    /// this module exists to remove — see the module docs for what id `0` is.
    #[must_use]
    pub fn resolve_wire_id(&self, name: &str) -> Option<i32> {
        if let Some(id) = entity_type_id(name) {
            return Some(id);
        }
        self.by_custom.get(name).map(EntityDisguise::wire_id)
    }

    /// The canonical vanilla type name `name` streams as, under the same
    /// resolution order as [`Self::resolve_wire_id`].
    ///
    /// For the **client** half of the disguise mapping: a client-only cosmetic entity needs
    /// a vanilla key to pick a mesh, texture and animation for, because every
    /// render-side lookup is keyed off the closed vanilla set.
    #[must_use]
    pub fn resolve_name<'a>(&'a self, name: &'a str) -> Option<&'a str> {
        if entity_type_id(name).is_some() {
            return Some(name);
        }
        self.by_custom.get(name).map(|d| d.streams_as())
    }

    /// Whether `name` is a registered custom kind rather than a vanilla type.
    #[must_use]
    pub fn is_custom(&self, name: &str) -> bool {
        self.by_custom.contains_key(name)
    }

    /// How many disguises are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_custom.len()
    }

    /// Whether nothing is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_custom.is_empty()
    }

    /// Every disguise, in custom-key order.
    pub fn iter(&self) -> impl Iterator<Item = &EntityDisguise> {
        self.by_custom.values()
    }
}

/// The namespace part of a joined `namespace:path`, or `minecraft` when the
/// string carries no namespace — matching vanilla's own default, so a bare
/// `"pig"` is treated as the vanilla type it means rather than slipping past the
/// reserved-namespace check.
fn namespace_of(name: &str) -> &str {
    match name.split_once(':') {
        Some((namespace, _)) => namespace,
        None => VANILLA_NAMESPACE,
    }
}

/// Why a disguise was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityDisguiseError {
    /// The custom kind is `minecraft:`-namespaced.
    ReservedNamespace(String),
    /// The vanilla type it would stream as does not exist.
    UnknownVanillaType {
        /// The custom kind being registered.
        custom: String,
        /// The unresolvable target.
        streams_as: String,
    },
    /// The custom kind is already registered.
    Duplicate(String),
}

impl core::fmt::Display for EntityDisguiseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ReservedNamespace(custom) => write!(
                f,
                "entity kind `{custom}` is in the reserved `{VANILLA_NAMESPACE}:` namespace"
            ),
            Self::UnknownVanillaType { custom, streams_as } => write!(
                f,
                "entity kind `{custom}` would stream as `{streams_as}`, which is not a vanilla \
                 entity type -- it would be encoded as network id 0 (`minecraft:acacia_boat`)"
            ),
            Self::Duplicate(custom) => {
                write!(f, "entity kind `{custom}` is already registered")
            }
        }
    }
}

impl std::error::Error for EntityDisguiseError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The fact the whole module is built around.** If this ever changes, the
    /// `unwrap_or(0)` fallback stops being a silent acacia boat and becomes a
    /// silent something-else — still wrong, but every doc here would be too.
    #[test]
    fn network_entity_type_id_zero_is_the_acacia_boat() {
        assert_eq!(
            crate::entity_types::entity_type_name(0),
            Some("minecraft:acacia_boat")
        );
        assert_eq!(entity_type_id("minecraft:acacia_boat"), Some(0));
    }

    #[test]
    fn a_registered_disguise_streams_as_its_vanilla_target() {
        let mut disguises = EntityDisguises::new();
        disguises
            .register("myplugin:sentry", "minecraft:armor_stand")
            .expect("armor_stand is real");

        let expected = entity_type_id("minecraft:armor_stand").expect("real type");
        assert_eq!(disguises.resolve_wire_id("myplugin:sentry"), Some(expected));
        assert_eq!(
            disguises.resolve_name("myplugin:sentry"),
            Some("minecraft:armor_stand")
        );
        assert!(disguises.is_custom("myplugin:sentry"));
    }

    /// **The gate the brief asks for: assert the streamed *type id*, not that an
    /// entity arrived.** An unregistered plugin key must resolve to `None` and in
    /// particular must NOT resolve to `Some(0)`.
    #[test]
    fn an_unregistered_custom_kind_resolves_to_none_and_never_to_the_boat() {
        let disguises = EntityDisguises::new();
        for name in [
            "myplugin:sentry",
            "myplugin:unregistered",
            "minecraft:not_a_real_entity",
            "",
        ] {
            let resolved = disguises.resolve_wire_id(name);
            assert_eq!(resolved, None, "{name} must be a definite miss");
            assert_ne!(
                resolved,
                Some(0),
                "{name} must never resolve to id 0 -- that is minecraft:acacia_boat"
            );
        }
    }

    /// The control for the above: a **real** vanilla type does resolve, so the
    /// test is measuring the miss rather than a function that always returns
    /// `None`.
    #[test]
    fn control_a_vanilla_type_still_resolves_to_its_own_id() {
        let disguises = EntityDisguises::new();
        for name in ["minecraft:pig", "minecraft:zombie", "minecraft:armor_stand"] {
            let resolved = disguises
                .resolve_wire_id(name)
                .unwrap_or_else(|| panic!("{name} is a real vanilla type"));
            assert_eq!(Some(resolved), entity_type_id(name));
            assert_eq!(disguises.resolve_name(name), Some(name));
            assert!(!disguises.is_custom(name));
        }
    }

    /// A disguise whose target does not exist is refused **at registration**,
    /// which is the design point: the failure moves off the wire and onto the
    /// call that caused it.
    #[test]
    fn a_disguise_targeting_an_unknown_vanilla_type_is_refused_at_registration() {
        let mut disguises = EntityDisguises::new();
        let err = disguises
            .register("myplugin:sentry", "minecraft:sentinel_of_doom")
            .expect_err("no such vanilla type");
        assert!(matches!(
            err,
            EntityDisguiseError::UnknownVanillaType { .. }
        ));
        assert!(disguises.is_empty(), "nothing may be registered on refusal");
        // And the message names the actual consequence, so a plugin author does
        // not have to know the table to understand the error.
        assert!(err.to_string().contains("acacia_boat"), "{err}");
    }

    /// A plugin key that is *itself* `minecraft:`-namespaced is refused: it would
    /// shadow a vanilla type, and every render-side lookup keyed off the vanilla
    /// registry would then disagree with this one.
    #[test]
    fn a_minecraft_namespaced_custom_kind_is_refused() {
        let mut disguises = EntityDisguises::new();
        for custom in ["minecraft:sentry", "sentry"] {
            let err = disguises
                .register(custom, "minecraft:armor_stand")
                .expect_err("minecraft: is the vanilla registry's");
            assert!(
                matches!(err, EntityDisguiseError::ReservedNamespace(_)),
                "{custom}: {err:?}"
            );
        }
        assert!(disguises.is_empty());
    }

    #[test]
    fn a_duplicate_custom_kind_is_refused() {
        let mut disguises = EntityDisguises::new();
        disguises
            .register("myplugin:sentry", "minecraft:armor_stand")
            .expect("first");
        let err = disguises
            .register("myplugin:sentry", "minecraft:zombie")
            .expect_err("second");
        assert!(matches!(err, EntityDisguiseError::Duplicate(_)));
        assert_eq!(
            disguises
                .get("myplugin:sentry")
                .map(EntityDisguise::streams_as),
            Some("minecraft:armor_stand"),
            "the first registration must be untouched"
        );
    }

    /// Unregistering restores the pre-registration answer exactly — the control
    /// the brief asks for in the "remove the registration and watch the effect
    /// vanish" shape, run rather than described.
    #[test]
    fn unregistering_makes_the_kind_unstreamable_again() {
        let mut disguises = EntityDisguises::new();
        disguises
            .register("myplugin:sentry", "minecraft:armor_stand")
            .expect("registers");
        assert!(disguises.resolve_wire_id("myplugin:sentry").is_some());

        let removed = disguises
            .unregister("myplugin:sentry")
            .expect("was registered");
        assert_eq!(removed.streams_as(), "minecraft:armor_stand");
        assert_eq!(
            disguises.resolve_wire_id("myplugin:sentry"),
            None,
            "with the registration gone the kind must be unstreamable, not a boat"
        );
        assert!(!disguises.is_custom("myplugin:sentry"));
    }
}
