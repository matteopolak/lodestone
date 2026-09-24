use std::collections::HashMap;
use std::sync::Arc;

use bevy_ecs::resource::Resource;
use uuid::Uuid;

use super::grants::{normalize_node, Permission, PermissionDefault, PermissionLevel, DEFAULT_PERMISSION};
use super::store::{PermissionStore, PermissionSubject};

/// What each declared node defaults to — a per-node default.
#[derive(Debug, Default)]
pub struct PermissionRegistry {
    declared: HashMap<String, PermissionDefault>,
    descriptions: HashMap<String, String>,
}

impl PermissionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Declare a node and its default, as a Bukkit plugin's `plugin.yml`
    /// `permissions:` block does.
    pub fn declare(&mut self, node: &str, default: PermissionDefault) {
        self.declared.insert(normalize_node(node), default);
    }

    /// Declare a node with a human description, for a future `/permissions`
    /// listing. The description is stored and never consulted by resolution.
    pub fn declare_described(
        &mut self,
        node: &str,
        default: PermissionDefault,
        description: impl Into<String>,
    ) {
        let node = normalize_node(node);
        self.declared.insert(node.clone(), default);
        self.descriptions.insert(node, description.into());
    }

    /// The declared default, or `None` if this node was never declared — which
    /// step 6 turns into [`DEFAULT_PERMISSION`].
    pub fn default_for(&self, node: &str) -> Option<PermissionDefault> {
        self.declared.get(&normalize_node(node)).copied()
    }

    pub fn description(&self, node: &str) -> Option<&str> {
        self.descriptions.get(&normalize_node(node)).map(|s| s.as_str())
    }

    pub fn declared_nodes(&self) -> impl Iterator<Item = (&str, PermissionDefault)> {
        self.declared.iter().map(|(k, v)| (k.as_str(), *v))
    }
}

/// Everything the built-in resolution needs to answer one query, handed to an
/// installed [`PermissionResolver`] so a plugin can make its own decision from
/// the same inputs.
pub struct PermissionQuery<'a> {
    pub subject: PermissionSubject,
    pub permission: &'a Permission,
    /// The subject's command level. [`PermissionLevel::Owners`] for
    /// [`PermissionSubject::Console`].
    pub level: PermissionLevel,
    pub registry: &'a PermissionRegistry,
    pub store: &'a PermissionStore,
}

impl std::fmt::Debug for PermissionQuery<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PermissionQuery")
            .field("subject", &self.subject)
            .field("permission", &self.permission)
            .field("level", &self.level)
            .finish_non_exhaustive()
    }
}

impl PermissionQuery<'_> {
    /// The node string, for the [`Permission::Atom`] case. `None` for a
    /// [`Permission::HasCommandLevel`] query, which has no node.
    pub fn node(&self) -> Option<&str> {
        match self.permission {
            Permission::Atom(node) => Some(node.as_str()),
            Permission::HasCommandLevel(_) => None,
        }
    }
}

/// The seam this module provides: a resolver trait so a permissions *plugin*
/// (matching the real-world pattern of delegating to LuckPerms) can override
/// the built-in op-level resolver entirely.
///
/// Returning `Some(bool)` decides the query outright. Returning `None` falls
/// through to the built-in order, so a plugin can own only the nodes it cares
/// about — a full takeover returns `Some` unconditionally.
pub trait PermissionResolver: Send + Sync {
    fn resolve(&self, query: &PermissionQuery<'_>) -> Option<bool>;
}

impl<F> PermissionResolver for F
where
    F: Fn(&PermissionQuery<'_>) -> Option<bool> + Send + Sync,
{
    fn resolve(&self, query: &PermissionQuery<'_>) -> Option<bool> {
        self(query)
    }
}

/// Vanilla's `LevelBasedPermissionSet`, exactly — provided so a caller that
/// wants *host* parity rather than *plugin* parity can have it.
///
/// This is **not** what [`Permissions`] does for atoms, and the difference is
/// deliberate: vanilla denies every atom except
/// `minecraft:commands/entity_selectors`, where Bukkit grants an undeclared
/// atom to any op. See the module doc's divergence section.
#[derive(Debug, Clone, Copy)]
pub struct LevelBasedPermissionSet {
    pub level: PermissionLevel,
}

impl LevelBasedPermissionSet {
    /// Vanilla's own level-based permission-set constructor.
    pub fn for_level(level: PermissionLevel) -> Self {
        Self { level }
    }

    /// Vanilla's node id for the one atom its level-based set special-cases:
    /// its own entity-selectors permission constant, an atom of
    /// `"commands/entity_selectors"`,
    /// which an `Identifier` with the default namespace renders as
    /// `minecraft:commands/entity_selectors`. Both spellings are accepted
    /// because vanilla's own constant omits the namespace at the call site.
    pub const COMMANDS_ENTITY_SELECTORS: &'static str = "commands/entity_selectors";

    /// Vanilla's own level-based permission-set check, transliterated.
    pub fn has_permission(&self, permission: &Permission) -> bool {
        match permission {
            Permission::HasCommandLevel(required) => self.level.is_equal_or_higher_than(*required),
            Permission::Atom(node) => {
                let node = normalize_node(node);
                let is_selectors = node == Self::COMMANDS_ENTITY_SELECTORS
                    || node == format!("minecraft:{}", Self::COMMANDS_ENTITY_SELECTORS);
                is_selectors && self.level.is_equal_or_higher_than(PermissionLevel::Gamemasters)
            }
        }
    }

    /// Two level-based sets collapse to the **higher** level rather than
    /// forming a union object.
    ///
    /// # A deliberate deviation from vanilla's literal code
    ///
    /// 26.2's own level-based permission-set union reads:
    ///
    /// ```text
    /// return this.level().isEqualOrHigherThan(otherSet.level()) ? otherSet : this;
    /// ```
    ///
    /// which returns the **lower**-level set when `this` is the higher one.
    /// That contradicts what `union` means everywhere else in the same file —
    /// vanilla's own permission-set-union check returns `true` if **any** member set
    /// holds the permission, i.e. a logical OR — so a union that *narrows* is
    /// inconsistent with its own interface. We implement the OR-consistent
    /// behaviour (keep the higher level) rather than transliterate what looks
    /// like an upstream bug, because a caller composing two sets and getting
    /// fewer permissions than either one had would be indefensible here even
    /// if it is what the jar does.
    ///
    /// This is the one place in this module that knowingly does not match the
    /// jar. If a live-oracle measurement ever shows vanilla's behaviour is
    /// load-bearing, flip it here and update this doc together —
    /// `vanilla_level_set_union_keeps_the_higher_level` is the test to change.
    pub fn union(self, other: Self) -> Self {
        if self.level.is_equal_or_higher_than(other.level) {
            self
        } else {
            other
        }
    }
}

/// The one resource a plugin asks. See the module doc for the resolution
/// order — that list is this type's specification.
#[derive(Resource, Default)]
pub struct Permissions {
    pub registry: PermissionRegistry,
    pub store: PermissionStore,
    resolver: Option<Arc<dyn PermissionResolver>>,
    /// Substituted for [`DEFAULT_PERMISSION`] at step 6. `None` means Bukkit's
    /// `Op`; [`Permissions::strict`] sets it to `False`.
    undeclared_default: Option<PermissionDefault>,
}

impl std::fmt::Debug for Permissions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Permissions")
            .field("registry", &self.registry)
            .field("store", &self.store)
            .field("resolver", &self.resolver.as_ref().map(|_| "<dyn PermissionResolver>"))
            .field("undeclared_default", &self.undeclared_default)
            .finish()
    }
}

impl Permissions {
    pub fn new() -> Self {
        Self::default()
    }

    /// Deny-by-default: an **undeclared** node resolves to `false` for
    /// everyone, including ops, instead of Bukkit's `Op`.
    ///
    /// Only step 6 changes. A declared node still uses its own default, and
    /// grants still win over both.
    pub fn strict() -> Self {
        Self {
            undeclared_default: Some(PermissionDefault::False),
            ..Self::default()
        }
    }

    /// Install a [`PermissionResolver`] that gets first refusal on every
    /// query. Replaces any previously installed one — there is deliberately no
    /// resolver *chain*, because two plugins silently disagreeing about a node
    /// is worse than one plugin obviously winning.
    pub fn set_resolver(&mut self, resolver: Arc<dyn PermissionResolver>) {
        self.resolver = Some(resolver);
    }


    pub fn has_resolver(&self) -> bool {
        self.resolver.is_some()
    }

    /// Declare a node's default. Convenience for `self.registry.declare`.
    pub fn declare(&mut self, node: &str, default: PermissionDefault) {
        self.registry.declare(node, default);
    }

    /// Grant a node to a player. Convenience for the store.
    pub fn grant(&mut self, player: Uuid, node: &str) {
        self.store.subject_mut(player).grants.allow(node);
    }

    /// Deny a node to a player (LuckPerms' `-node`).
    pub fn deny(&mut self, player: Uuid, node: &str) {
        self.store.subject_mut(player).grants.deny(node);
    }

    /// The common case: does this subject hold this dotted node?
    pub fn has(&self, subject: PermissionSubject, node: &str) -> bool {
        self.check(subject, &Permission::Atom(node.to_string()))
    }

    /// Is this subject at `level` or higher? The op-level accessor,
    /// expressed through the same resolution order so an installed resolver
    /// can override *this* too — a LuckPerms-equivalent that wants to grant
    /// gamemaster powers by node rather than by op list can.
    pub fn has_level(&self, subject: PermissionSubject, level: PermissionLevel) -> bool {
        self.check(subject, &Permission::HasCommandLevel(level))
    }

    /// The subject's command level. `Owners` for the console.
    pub fn level(&self, subject: PermissionSubject) -> PermissionLevel {
        match subject {
            PermissionSubject::Console => PermissionLevel::Owners,
            PermissionSubject::Player(id) => self.store.level(id),
        }
    }

    /// The full resolution, in the order the module doc specifies.
    pub fn check(&self, subject: PermissionSubject, permission: &Permission) -> bool {
        let level = self.level(subject);

        // Step 1 — an installed resolver gets first refusal, for every
        // subject including the console. A permissions plugin that wants to
        // deny the console something must be able to.
        if let Some(resolver) = &self.resolver {
            let query = PermissionQuery {
                subject,
                permission,
                level,
                registry: &self.registry,
                store: &self.store,
            };
            if let Some(decided) = resolver.resolve(&query) {
                return decided;
            }
        }

        // The console holds everything — vanilla's `ALL_PERMISSIONS`.
        if matches!(subject, PermissionSubject::Console) {
            return true;
        }

        // A level query is answered by the level alone. There is no node to
        // match a grant against, and no default to consult.
        let node = match permission {
            Permission::HasCommandLevel(required) => {
                return level.is_equal_or_higher_than(*required);
            }
            Permission::Atom(node) => normalize_node(node),
        };

        // Steps 2–4 — the most specific matching grant, deny-over-allow at
        // equal specificity, own-over-inherited after that.
        if let PermissionSubject::Player(id) = subject {
            if let Some(matched) = self.store.collect_grants(id, &node) {
                return matched.grant.as_bool();
            }
        }

        // Steps 5–6 — the declared default, else the undeclared fallback.
        let default = self
            .registry
            .default_for(&node)
            .or(self.undeclared_default)
            .unwrap_or(DEFAULT_PERMISSION);
        default.value(level.is_op())
    }
}
