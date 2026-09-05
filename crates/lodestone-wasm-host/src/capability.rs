//! What a plugin may do — the capability vocabulary a plugin manifest
//! declares against, and the thing the host's `Linker` and conductor enforce.
//!
//! # Two enforcement mechanisms, and knowing which one you are relying on
//!
//! This is the single most important thing to understand before adding a
//! capability, because the two halves have very different security properties and
//! they look identical in a manifest:
//!
//! | kind | example | enforced by | what a lying manifest gets |
//! |---|---|---|---|
//! | **import** | [`Capability::FsRead`] | the wasmtime `Linker` — the interface is simply absent | instantiation fails: *"component imports instance `lodestone:plugin/filesystem@0.2.0`, but a matching implementation was not found in the linker"* |
//! | **data-flow** | [`Capability::ObserveChat`], [`Capability::ActChat`] | the host's own conductor, in Rust | the events are never lifted, or the actions are counted and dropped |
//!
//! An **import** capability is structurally unforgeable: the guest cannot call a
//! function that was never linked, and it cannot even finish instantiating if it
//! references one. That is the strong kind, and anything genuinely dangerous
//! (filesystem, network, subprocess) must be modelled as an import so it lands in
//! that column.
//!
//! A **data-flow** capability is a host-side filter. It is not weaker in
//! practice — the guest has no other route to the event stream or to
//! `ActionQueue` — but it *is* enforced by our code rather than by the runtime,
//! so it needs its own test with a control. `tests/capability_denial.rs` covers
//! both columns, and deliberately does so with a fixture that misbehaves rather
//! than with a well-behaved one that happens not to try.
//!
//! # How to add one
//!
//! 1. Add the variant, its wire name in [`Capability::as_str`], and its parse arm
//!    in [`Capability::parse`]. The three are kept in one file so they cannot
//!    drift; `tests/capability_names.rs` asserts the round trip over
//!    [`Capability::ALL`], which is the guard that a new variant cannot be added
//!    to only two of the three places.
//! 2. Decide which column above it is in. If it is an import, add the interface
//!    to the `.wit` world and gate its `add_to_linker` in
//!    [`crate::host::PluginHost`]. If it is data-flow, gate it where the data is
//!    lifted or lowered in `crate::abi`.
//! 3. Decide whether [`CapabilitySet::default_policy`] grants it. **The default
//!    for anything in the import column is "no".**

use std::collections::BTreeSet;
use std::fmt;

/// One thing a plugin may be permitted to do.
///
/// The wire form (what appears in a `plugin.toml`) is
/// `family:verb` — `observe:chat`, `act:chat`, `fs:read` — with `log` as the one
/// unqualified name, because it is granted unconditionally and has no siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Capability {
    /// Write to the host's `tracing` sink, tagged with the plugin's name.
    /// Granted to every plugin: the host owns the sink, so this reaches no file
    /// and no terminal the host was not already writing to.
    Log,
    /// Receive `ClientEvent::Chat` as [`crate::Event::Chat`].
    ObserveChat,
    /// Receive `ClientEvent::HealthChanged`.
    ObserveHealth,
    /// Receive canonical item identity/count changes for native player-inventory
    /// slots outside an open container.
    ObserveInventory,
    /// Receive `ClientEvent::SectionBlocksChanged`.
    ObserveBlocks,
    /// Push `ClientAction::SendChat` / `SendCommand` onto `ActionQueue`.
    ///
    /// One capability for both because they are the same power: `SendCommand`
    /// is `SendChat` with the slash already stripped, and a server cannot tell
    /// a plugin that may chat from one that may run the commands it is allowed
    /// to type.
    ActChat,
    /// Push `ClientAction::SwingArm`.
    ActInteract,
    /// Install or remove the local player's copied look intent.
    ///
    /// This is deliberately distinct from `act:interact`: changing a player's
    /// facing changes simulation and movement-packet output, not merely a
    /// cosmetic arm animation.
    ActLook,
    /// Override this tick's copied local-player movement intent.
    ///
    /// The guest cannot construct a position or packet: physics consumes the
    /// copied intent and the controller remains the sole egress producer.
    ActMovement,
    /// Start, continue, retarget, or abort a local-player block break through
    /// the shell-owned mining lifecycle.
    ActBreak,
    /// Request one local-player block placement through the shell-owned
    /// placement lifecycle.
    ///
    /// This does not grant a block-state, hand, prediction sequence, or packet
    /// constructor. The guest supplies only the target cell and face; the shell
    /// applies its normal reach, inventory, collision, veto, prediction, and
    /// egress checks.
    ActPlace,
    /// Request a one-shot selected-hotbar-slot update through the shell-owned
    /// selection and carried-item echo path.
    ///
    /// The guest supplies only a slot number. The shell still owns the range
    /// gate, selected-slot write, and protocol egress.
    ActSelectSlot,
    /// Request one left/right pickup click through the shell-owned menu predictor.
    ///
    /// The guest supplies only a bounded menu slot and button. The shell validates
    /// the slot against the active menu, then owns prediction, vetoes, state ids,
    /// and protocol egress.
    ActInventoryClick,
    /// Request one shift-click quick move through the shell-owned menu predictor.
    ///
    /// This is intentionally separate from [`Self::ActInventoryClick`]: a policy
    /// that permits pickup/place does not implicitly permit the wider transfer
    /// operation. The guest supplies only a bounded menu slot; the shell owns
    /// the transfer order, validation, prediction, vetoes, and egress.
    ActInventoryQuickMove,
    /// Request one number-key swap through the shell-owned menu predictor.
    ///
    /// This is intentionally separate from [`Self::ActInventoryClick`] and
    /// [`Self::ActInventoryQuickMove`]: it can exchange a live menu slot with a
    /// selected hotbar position, while the shell still owns slot validation,
    /// prediction, vetoes, and protocol egress.
    ActInventoryHotbarSwap,
    /// Receive a generation-bounded outcome after a WASM or native placement
    /// attempt resolves.
    ///
    /// The event exposes the normal finite status/rejection vocabulary rather
    /// than a world handle or an unbounded error string.
    ObservePlace,
    /// Receive changed, bounded outcomes from the local-player break lifecycle.
    ObserveBreak,
    /// Decide whether the client may commit one of its typed player actions.
    ///
    /// This is data-flow gated at the synchronous host callback: a guest without
    /// it is never called at an action-veto point.
    VetoActions,
    /// Register a guest-owned root command with the native command registry.
    ///
    /// This is data-flow gated while the host installs commands: a guest may
    /// declare command specs, but it receives no handler unless this capability
    /// was granted.
    RegisterCommands,
    /// Read a file through the `lodestone:plugin/filesystem` interface.
    ///
    /// **Never in [`CapabilitySet::default_policy`].** This is the import-column
    /// capability the denial gate is built around; see this module's table.
    FsRead,
    /// Schedule delayed or repeating guest callbacks through the host tick.
    ///
    /// **Never in [`CapabilitySet::default_policy`].** The scheduler is exposed
    /// as an import so a guest that does not declare it fails to instantiate.
    ScheduleTasks,
}

impl Capability {
    /// Every variant, in declaration order. The source of truth for the
    /// name-round-trip guard, so a new variant is covered by it automatically —
    /// which is the point, since the failure mode being guarded against is
    /// *forgetting* to update one of three places.
    pub const ALL: &'static [Self] = &[
        Self::Log,
        Self::ObserveChat,
        Self::ObserveHealth,
        Self::ObserveInventory,
        Self::ObserveBlocks,
        Self::ActChat,
        Self::ActInteract,
        Self::ActLook,
        Self::ActMovement,
        Self::ActBreak,
        Self::ActPlace,
        Self::ActSelectSlot,
        Self::ActInventoryClick,
        Self::ActInventoryQuickMove,
        Self::ActInventoryHotbarSwap,
        Self::ObservePlace,
        Self::ObserveBreak,
        Self::VetoActions,
        Self::RegisterCommands,
        Self::FsRead,
        Self::ScheduleTasks,
    ];

    /// The name that appears in a `plugin.toml`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Log => "log",
            Self::ObserveChat => "observe:chat",
            Self::ObserveHealth => "observe:health",
            Self::ObserveInventory => "observe:inventory",
            Self::ObserveBlocks => "observe:blocks",
            Self::ActChat => "act:chat",
            Self::ActInteract => "act:interact",
            Self::ActLook => "act:look",
            Self::ActMovement => "act:movement",
            Self::ActBreak => "act:break",
            Self::ActPlace => "act:place",
            Self::ActSelectSlot => "act:select-slot",
            Self::ActInventoryClick => "act:inventory-click",
            Self::ActInventoryQuickMove => "act:inventory-quick-move",
            Self::ActInventoryHotbarSwap => "act:inventory-hotbar-swap",
            Self::ObservePlace => "observe:place",
            Self::ObserveBreak => "observe:break",
            Self::VetoActions => "veto:actions",
            Self::RegisterCommands => "commands:register",
            Self::FsRead => "fs:read",
            Self::ScheduleTasks => "schedule:tasks",
        }
    }

    /// Parse a manifest capability name.
    ///
    /// `None` means *unrecognised*, which a manifest loader must turn into a
    /// loud rejection rather than a silent skip — the case of a manifest from a
    /// newer ABI version than the host supports. Dropping an unknown name
    /// would grant a plugin less than it asked for and let it run anyway, which
    /// is the worst of both: it does not work, and nothing says why.
    #[must_use]
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == name)
    }

    /// Whether this capability is enforced by the wasmtime `Linker` (an
    /// *import*) rather than by the host's own conductor (*data-flow*). See this
    /// module's table.
    #[must_use]
    pub const fn is_import(self) -> bool {
        match self {
            Self::Log | Self::FsRead | Self::ScheduleTasks => true,
            Self::ObserveChat
            | Self::ObserveHealth
            | Self::ObserveInventory
            | Self::ObserveBlocks
            | Self::ActChat
            | Self::ActInteract
            | Self::ActLook
            | Self::ActMovement
            | Self::ActBreak
            | Self::ActPlace
            | Self::ActSelectSlot
            | Self::ActInventoryClick
            | Self::ActInventoryQuickMove
            | Self::ActInventoryHotbarSwap
            | Self::ObservePlace
            | Self::ObserveBreak
            | Self::VetoActions
            | Self::RegisterCommands => false,
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A set of capabilities — either what a plugin *requests* (from its manifest)
/// or what the host *grants* (its policy). The two are compared with
/// [`CapabilitySet::missing_from`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapabilitySet(BTreeSet<Capability>);

impl CapabilitySet {
    /// The empty set — a plugin that declared nothing. Note this is *not* the
    /// default policy; see [`Self::default_policy`].
    #[must_use]
    pub fn empty() -> Self {
        Self(BTreeSet::new())
    }

    /// What the host grants unless an operator says otherwise: the ordinary
    /// observe/act vocabulary, but not filesystem access, task scheduling,
    /// command registration, or the opt-in inventory mutations.
    ///
    /// The asymmetry is the whole policy, stated in one place. Observing chat and
    /// pushing a chat action are things a plugin is *for*; reading the user's
    /// filesystem is not, and `docs/plugin-api.md`'s comparison table already
    /// promises it is "denied unless a capability is granted". A grant is
    /// therefore an explicit operator act, which is why this function enumerates
    /// rather than subtracting from `ALL` — a new dangerous capability added to
    /// `ALL` must not be granted by an omission.
    #[must_use]
    pub fn default_policy() -> Self {
        Self::from_iter([
            Capability::Log,
            Capability::ObserveChat,
            Capability::ObserveHealth,
            Capability::ObserveBlocks,
            Capability::ActChat,
            Capability::ActInteract,
            Capability::VetoActions,
        ])
    }

    /// Every capability, including the dangerous ones. For a host that has
    /// decided to trust a plugin, and for the *control* arm of the denial gate —
    /// a test that never grants anything cannot tell "refused" from "never
    /// attempted".
    #[must_use]
    pub fn permissive() -> Self {
        Self::from_iter(Capability::ALL.iter().copied())
    }

    #[must_use]
    pub fn contains(&self, capability: Capability) -> bool {
        self.0.contains(&capability)
    }

    pub fn insert(&mut self, capability: Capability) -> bool {
        self.0.insert(capability)
    }

    /// Add every capability in `other` to this set.
    ///
    /// Discovery policy uses this to add a narrowly configured exception to a
    /// host's baseline policy. It can only add permissions: the host baseline
    /// remains the fail-closed floor for every plugin without an exception.
    pub fn extend_from(&mut self, other: &Self) {
        self.0.extend(other.iter());
    }

    /// The capabilities in `self` that `policy` does not grant, in wire-name
    /// order. Empty means the request is satisfiable.
    ///
    /// Returns *all* of them rather than the first, because a rejection message
    /// naming one capability sends the operator round the loop once per missing
    /// grant.
    #[must_use]
    pub fn missing_from(&self, policy: &Self) -> Vec<Capability> {
        self.0.iter().copied().filter(|c| !policy.contains(*c)).collect()
    }

    #[must_use]
    pub fn iter(&self) -> impl Iterator<Item = Capability> + '_ {
        self.0.iter().copied()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl FromIterator<Capability> for CapabilitySet {
    fn from_iter<I: IntoIterator<Item = Capability>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl fmt::Display for CapabilitySet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut first = true;
        for c in &self.0 {
            if !first {
                f.write_str(", ")?;
            }
            first = false;
            f.write_str(c.as_str())?;
        }
        if first {
            f.write_str("(none)")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every variant's wire name parses back to itself. The guard against a new
    /// variant reaching `ALL` and `as_str` but not `parse` — or worse, reaching
    /// `as_str` with a name that collides with another's.
    #[test]
    fn every_capability_name_round_trips() {
        for &c in Capability::ALL {
            assert_eq!(
                Capability::parse(c.as_str()),
                Some(c),
                "`{}` did not parse back to {c:?}",
                c.as_str()
            );
        }
        let distinct: BTreeSet<&str> = Capability::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(
            distinct.len(),
            Capability::ALL.len(),
            "two capabilities share a wire name"
        );
    }

    /// An unknown name is `None`, not a silently-dropped grant.
    #[test]
    fn an_unknown_capability_name_is_rejected() {
        assert_eq!(Capability::parse("fs:write"), None);
        assert_eq!(Capability::parse("observe:everything"), None);
        assert_eq!(Capability::parse(""), None);
    }

    /// The policy asymmetry, asserted rather than described: the default grants
    /// the observe/act vocabulary and withholds opt-in capabilities.
    #[test]
    fn the_default_policy_withholds_import_capabilities() {
        let policy = CapabilitySet::default_policy();
        assert!(!policy.contains(Capability::FsRead), "fs:read must not be granted by default");
        assert!(
            !policy.contains(Capability::ScheduleTasks),
            "schedule:tasks must not be granted by default"
        );
        assert!(
            !policy.contains(Capability::RegisterCommands),
            "commands:register must not be granted by default"
        );
        assert!(
            !policy.contains(Capability::ActSelectSlot),
            "act:select-slot must not be granted by default"
        );
        assert!(
            !policy.contains(Capability::ActInventoryClick),
            "act:inventory-click must not be granted by default"
        );
        assert!(
            !policy.contains(Capability::ActInventoryHotbarSwap),
            "act:inventory-hotbar-swap must not be granted by default"
        );
        assert!(policy.contains(Capability::ObserveChat));
        assert!(policy.contains(Capability::ActChat));
        assert!(policy.contains(Capability::Log));

        // The control for the assertion above: the permissive policy *does*
        // grant it, so "absent" above is a decision and not an artefact of
        // `contains` always answering false.
        assert!(CapabilitySet::permissive().contains(Capability::FsRead));
        assert!(CapabilitySet::permissive().contains(Capability::ScheduleTasks));
        assert!(CapabilitySet::permissive().contains(Capability::RegisterCommands));
        assert!(CapabilitySet::permissive().contains(Capability::ActSelectSlot));
    }

    /// `missing_from` reports every shortfall, not just the first.
    #[test]
    fn missing_from_reports_all_shortfalls() {
        let request = CapabilitySet::from_iter([Capability::FsRead, Capability::ActChat]);
        let policy = CapabilitySet::from_iter([Capability::Log]);
        let missing = request.missing_from(&policy);
        assert_eq!(missing.len(), 2, "got {missing:?}");
        assert!(missing.contains(&Capability::FsRead));
        assert!(missing.contains(&Capability::ActChat));

        // Control: against a policy that grants both, nothing is missing.
        assert!(request.missing_from(&CapabilitySet::permissive()).is_empty());
    }

    /// The import/data-flow split is a fact about each variant, and the
    /// dangerous one is in the strong column.
    #[test]
    fn dangerous_host_services_are_import_capabilities() {
        assert!(Capability::FsRead.is_import());
        assert!(Capability::ScheduleTasks.is_import());
        assert!(!Capability::ObserveChat.is_import());
        assert!(!Capability::ActChat.is_import());
    }
}
