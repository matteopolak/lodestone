//! Proposal-backed, Paper-shaped event adjudication.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use bevy_ecs::prelude::Resource;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ResourceKey, Vec3};

use super::proposals::{ProposalVerdict, ServerProposalAction};

/// Event kinds currently backed by the server proposal vocabulary.
///
/// `PlayerInteract` is intentionally present as a known-but-unsupported kind:
/// attempting to register it fails explicitly until that packet path has a
/// proposal owner. This keeps a successful registration meaningful.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum PaperEventKind {
    /// A proposed mob spawn.
    EntitySpawn,
    /// A proposed mutation of a block in a resident column.
    ResidentBlockChange,
    /// A player interaction, which has no proposal owner yet.
    PlayerInteract,
}

impl PaperEventKind {
    /// Whether this event kind has a proposal-backed dispatch path.
    #[must_use]
    pub const fn supported(self) -> bool {
        matches!(self, Self::EntitySpawn | Self::ResidentBlockChange)
    }
}

/// Listener priority, ordered from earliest to latest dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum PaperEventPriority {
    Lowest,
    Low,
    Normal,
    High,
    Highest,
    Monitor,
}

/// A mutable observation passed to one registered listener at a time.
#[derive(Debug, Clone, PartialEq)]
pub enum PaperEvent {
    /// A mob spawn proposal.
    EntitySpawn {
        /// The version-free entity key.
        entity_type: ResourceKey,
        /// The proposed position.
        pos: Vec3,
        /// Whether the proposal has been cancelled.
        cancelled: bool,
    },
    /// A resident block mutation proposal.
    ResidentBlockChange {
        /// The target block position.
        pos: BlockPos,
        /// The replacement state.
        state: StateId,
        /// Whether the proposal has been cancelled.
        cancelled: bool,
    },
}

impl PaperEvent {
    /// The registered kind for this event.
    #[must_use]
    pub const fn kind(&self) -> PaperEventKind {
        match self {
            Self::EntitySpawn { .. } => PaperEventKind::EntitySpawn,
            Self::ResidentBlockChange { .. } => PaperEventKind::ResidentBlockChange,
        }
    }

    /// Marks the event as cancelled.
    pub fn cancel(&mut self) {
        match self {
            Self::EntitySpawn { cancelled, .. }
            | Self::ResidentBlockChange { cancelled, .. } => *cancelled = true,
        }
    }

    /// Whether a listener has cancelled this event.
    #[must_use]
    pub const fn is_cancelled(&self) -> bool {
        match self {
            Self::EntitySpawn { cancelled, .. }
            | Self::ResidentBlockChange { cancelled, .. } => *cancelled,
        }
    }

    fn from_action(action: &ServerProposalAction) -> Option<Self> {
        match action {
            ServerProposalAction::SpawnMob { entity_type, pos } => Some(Self::EntitySpawn {
                entity_type: entity_type.clone(),
                pos: *pos,
                cancelled: false,
            }),
            ServerProposalAction::SetResidentBlock { pos, state } => {
                Some(Self::ResidentBlockChange {
                    pos: *pos,
                    state: *state,
                    cancelled: false,
                })
            }
            ServerProposalAction::NaturalSpawnMob { .. }
            | ServerProposalAction::DespawnMob { .. } => None,
        }
    }

    fn into_action(self) -> ServerProposalAction {
        match self {
            Self::EntitySpawn { entity_type, pos, .. } => {
                ServerProposalAction::SpawnMob { entity_type, pos }
            }
            Self::ResidentBlockChange { pos, state, .. } => {
                ServerProposalAction::SetResidentBlock { pos, state }
            }
        }
    }
}

/// Why listener registration was rejected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperEventRegistrationError {
    /// The proposal vocabulary does not yet own this event kind.
    Unsupported(PaperEventKind),
}

struct Registration {
    kind: PaperEventKind,
    priority: PaperEventPriority,
    order: u64,
    name: &'static str,
    listener: Arc<dyn Fn(&mut PaperEvent) + Send + Sync + 'static>,
}

/// A server-local event bus whose events are resolved into proposal verdicts.
#[derive(Resource, Default)]
pub struct PaperEventBus {
    registrations: Vec<Registration>,
    next_order: u64,
    failures: Vec<PaperEventFailure>,
}

impl std::fmt::Debug for PaperEventBus {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaperEventBus")
            .field("registrations", &self.registrations.len())
            .field("failures", &self.failures)
            .finish()
    }
}

/// A listener that panicked while handling an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaperEventFailure {
    /// The event kind being dispatched.
    pub kind: PaperEventKind,
    /// The listener's registration name.
    pub listener: &'static str,
}

impl PaperEventBus {
    /// Registers one listener. Listeners are invoked in priority then
    /// registration order, and every listener sees prior mutations.
    pub fn register(
        &mut self,
        kind: PaperEventKind,
        priority: PaperEventPriority,
        name: &'static str,
        listener: impl Fn(&mut PaperEvent) + Send + Sync + 'static,
    ) -> Result<(), PaperEventRegistrationError> {
        if !kind.supported() {
            return Err(PaperEventRegistrationError::Unsupported(kind));
        }
        let order = self.next_order;
        self.next_order = self
            .next_order
            .checked_add(1)
            .expect("paper event registration order exhausted");
        self.registrations.push(Registration {
            kind,
            priority,
            order,
            name,
            listener: Arc::new(listener),
        });
        Ok(())
    }

    /// Removes all recorded listener failures and returns them to the owner.
    pub fn take_failures(&mut self) -> Vec<PaperEventFailure> {
        std::mem::take(&mut self.failures)
    }

    /// Dispatches listeners for one proposal and returns a verdict when that
    /// proposal has at least one registered listener. Unsupported proposal
    /// variants are left to their existing consumers.
    pub fn adjudicate(&mut self, action: &ServerProposalAction) -> Option<ProposalVerdict> {
        let mut event = PaperEvent::from_action(action)?;
        let kind = event.kind();
        let mut listeners: Vec<_> = self
            .registrations
            .iter()
            .filter(|registration| registration.kind == kind)
            .map(|registration| {
                (
                    registration.priority,
                    registration.order,
                    registration.name,
                    Arc::clone(&registration.listener),
                )
            })
            .collect();
        listeners.sort_by_key(|(priority, order, _, _)| (*priority, *order));
        if listeners.is_empty() {
            return None;
        }
        for (_, _, name, listener) in listeners {
            if catch_unwind(AssertUnwindSafe(|| listener(&mut event))).is_err() {
                self.failures.push(PaperEventFailure { kind, listener: name });
            }
        }
        if event.is_cancelled() {
            Some(ProposalVerdict::Deny)
        } else {
            let replacement = event.into_action();
            if replacement == *action {
                Some(ProposalVerdict::Allow)
            } else {
                Some(ProposalVerdict::Replace(replacement))
            }
        }
    }
}
