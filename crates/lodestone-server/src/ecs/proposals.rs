//! Native-plugin adjudication of server actions on `TickSet::Adjudicate`.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Mutex;
use std::time::Duration;

use bevy_ecs::message::{Message, MessageReader, MessageWriter};
use bevy_ecs::prelude::{ResMut, Resource};
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockFace, BlockPos, GameMode, Hand, ResourceKey, Vec3};
use uuid::Uuid;

use super::{TickSet, paper_events::PaperEventBus};

const QUEUE_CAPACITY: usize = 64;
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(1);

/// A server action a native plugin may observe and adjudicate.
#[derive(Debug, Clone, PartialEq, Message)]
pub struct ServerProposal {
    id: u64,
    pub action: ServerProposalAction,
}

impl ServerProposal {
    /// The proposal's id, stable for this one adjudication pass.
    #[must_use]
    pub fn id(&self) -> u64 { self.id }
}

/// The version-free vocabulary a proposal can carry.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerProposalAction {
    /// Spawn this entity kind at this position through `IntegratedServer`.
    SpawnMob { entity_type: ResourceKey, pos: Vec3 },
    /// Materialize this naturally selected species with its spawn-list category.
    NaturalSpawnMob {
        entity_type: ResourceKey,
        pos: Vec3,
        category: crate::mob_spawn::MobCategory,
    },
    /// Remove this exact live mob id through `IntegratedServer`.
    DespawnMob { id: i32 },
    /// Change one connected player's game mode through their authoritative
    /// connection, after native plugins have had a chance to allow or deny it.
    SetPlayerGameMode { target: Uuid, mode: GameMode },
    /// Replace one already-resident block through `IntegratedServer`.
    ///
    /// The state id is validated at the boundary by [`StateId`], so an
    /// adjudicator never has to defend a raw registry integer. The resident
    /// requirement is intentional: a plugin proposal must not turn into an
    /// unbounded chunk load or generation request.
    SetResidentBlock { pos: BlockPos, state: StateId },
    /// A player interaction against a block face, before the interaction's
    /// world mutation or use result is applied.
    PlayerInteract {
        pos: BlockPos,
        face: BlockFace,
        hand: Hand,
        using_secondary_action: bool,
    },
    /// A player has earned a block break and the connection is about to apply
    /// the authoritative replacement. The block state is carried so a
    /// listener can identify the target without reading the world or causing
    /// a cold-column load.
    BlockBreak {
        pos: BlockPos,
        state: StateId,
        breaker: Uuid,
    },
    /// Replace an ordered resident block batch atomically through
    /// `IntegratedServer`.
    ///
    /// The boolean controls only the bridge's resident-change observer
    /// notification. Every target and state is preflighted before the first
    /// source mutation, and a batch is never decomposed into scalar proposals.
    SetResidentBlockBatch {
        writes: Vec<(BlockPos, StateId)>,
        notify_listeners: bool,
    },
}

/// A plugin's answer to a [`ServerProposal`].
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalVerdict {
    /// Leave the proposed action unchanged.
    Allow,
    /// Refuse the action; the caller receives [`ProposalRefusal::Denied`].
    Deny,
    /// Apply a replacement action instead of the original one.
    Replace(ServerProposalAction),
}

/// Why an externally proposed action did not become world state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProposalRefusal {
    /// A plugin denied the proposal.
    Denied,
    /// The tick task did not consume the request within the bounded wait.
    TimedOut,
    /// The bounded ingress queue is full or the tick task has stopped.
    Unavailable,
    /// A replacement changed the proposal into an action this caller cannot apply.
    MismatchedAction,
}

/// Why a checked spawn did not become a mob.
pub type SpawnProposalRefusal = ProposalRefusal;

/// Why a checked despawn did not remove a mob.
pub type DespawnProposalRefusal = ProposalRefusal;

struct Request {
    action: ServerProposalAction,
    reply: tokio::sync::oneshot::Sender<Result<ServerProposalAction, ProposalRefusal>>,
}

/// Cloneable ingress for callers outside the tick-owned ECS world.
#[derive(Clone, Debug, Resource)]
pub struct ServerProposalHandle {
    sender: SyncSender<Request>,
}

impl ServerProposalHandle {
    /// Submit an action and await one `Drain → Adjudicate → Apply` pass.
    async fn submit(
        &self,
        action: ServerProposalAction,
    ) -> Result<ServerProposalAction, ProposalRefusal> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        match self.sender.try_send(Request { action, reply }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                return Err(ProposalRefusal::Unavailable);
            }
        }
        tokio::time::timeout(RESPONSE_TIMEOUT, receiver)
            .await
            .map_or(Err(ProposalRefusal::TimedOut), |outcome| {
                outcome.unwrap_or(Err(ProposalRefusal::Unavailable))
            })
    }

    /// Submit a spawn and await one `Drain → Adjudicate → Apply` pass.
    pub async fn spawn_mob(
        &self,
        entity_type: ResourceKey,
        pos: Vec3,
    ) -> Result<ServerProposalAction, SpawnProposalRefusal> {
        self.submit(ServerProposalAction::SpawnMob { entity_type, pos }).await
    }

    /// Submit a despawn and await one `Drain → Adjudicate → Apply` pass.
    pub async fn despawn_mob(&self, id: i32) -> Result<ServerProposalAction, DespawnProposalRefusal> {
        self.submit(ServerProposalAction::DespawnMob { id }).await
    }

    /// Submit one bounded resident-block mutation and await one
    /// `Drain → Adjudicate → Apply` pass.
    pub async fn set_resident_block(
        &self,
        pos: BlockPos,
        state: StateId,
    ) -> Result<ServerProposalAction, ProposalRefusal> {
        self.submit(ServerProposalAction::SetResidentBlock { pos, state }).await
    }

    /// Submit one connected-player game-mode change and await one
    /// `Drain → Adjudicate → Apply` pass.
    pub async fn set_player_game_mode(
        &self,
        target: Uuid,
        mode: GameMode,
    ) -> Result<ServerProposalAction, ProposalRefusal> {
        self.submit(ServerProposalAction::SetPlayerGameMode { target, mode }).await
    }

    /// Submit a player interaction and await its adjudicated action.
    pub async fn player_interact(
        &self,
        pos: BlockPos,
        face: BlockFace,
        hand: Hand,
        using_secondary_action: bool,
    ) -> Result<ServerProposalAction, ProposalRefusal> {
        self.submit(ServerProposalAction::PlayerInteract {
            pos,
            face,
            hand,
            using_secondary_action,
        })
        .await
    }

    /// Submit one ordered, atomically preflighted resident-block batch.
    pub async fn set_resident_block_batch(
        &self,
        writes: Vec<(BlockPos, StateId)>,
        notify_listeners: bool,
    ) -> Result<ServerProposalAction, ProposalRefusal> {
        self.submit(ServerProposalAction::SetResidentBlockBatch {
            writes,
            notify_listeners,
        }).await
    }

    /// Submit a validated player block break and await its adjudicated action.
    pub async fn block_break(
        &self,
        pos: BlockPos,
        state: StateId,
        breaker: Uuid,
    ) -> Result<ServerProposalAction, ProposalRefusal> {
        self.submit(ServerProposalAction::BlockBreak { pos, state, breaker })
            .await
    }
}

/// Per-proposal decisions. Lower priorities win; ties keep the system that ran
/// first, which is deterministic when plugins order their Adjudicate systems.
#[derive(Debug, Resource, Default)]
pub struct ServerProposalDecisions {
    next_order: u64,
    decisions: HashMap<u64, (i32, u64, ProposalVerdict)>,
}

impl ServerProposalDecisions {
    /// Record a non-allow verdict. Returns whether it is currently decisive.
    pub fn decide(&mut self, id: u64, priority: i32, verdict: ProposalVerdict) -> bool {
        if matches!(verdict, ProposalVerdict::Allow) { return false; }
        let order = self.next_order;
        self.next_order = self.next_order.checked_add(1).expect("proposal decision order exhausted");
        let replace = self.decisions.get(&id).is_none_or(|(old_priority, old_order, _)| {
            (priority, order) < (*old_priority, *old_order)
        });
        if replace { self.decisions.insert(id, (priority, order, verdict)); }
        replace
    }

    fn take(&mut self, id: u64) -> ProposalVerdict {
        self.decisions.remove(&id).map_or(ProposalVerdict::Allow, |(_, _, verdict)| verdict)
    }
}

struct Pending {
    id: u64,
    action: ServerProposalAction,
    reply: Option<tokio::sync::oneshot::Sender<Result<ServerProposalAction, ProposalRefusal>>>,
}

/// A stable ticket for a proposal staged by the tick owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServerProposalTicket(u64);

/// A resolution produced by the shared apply pass.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerProposalResolution {
    ticket: ServerProposalTicket,
    pub outcome: Result<ServerProposalAction, ProposalRefusal>,
}

impl ServerProposalResolution {
    /// The ticket returned by [`ServerProposalQueue::stage`].
    #[must_use]
    pub fn ticket(&self) -> ServerProposalTicket { self.ticket }
}

/// Tick-owned staging and result queue for engine proposals.
///
/// The tick owner stages natural actions before running `GameTick`, then takes
/// their resolutions before acquiring the mob simulation lock to apply them.
/// Plugins never receive a mob lock through this resource.
#[derive(Resource)]
pub struct ServerProposalQueue {
    receiver: Mutex<Receiver<Request>>,
    next_id: u64,
    staged: Vec<Pending>,
    pending: Vec<Pending>,
    resolved: Vec<ServerProposalResolution>,
}

impl std::fmt::Debug for ServerProposalQueue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerProposalQueue")
            .field("next_id", &self.next_id)
            .field("staged", &self.staged.len())
            .field("pending", &self.pending.len())
            .field("resolved", &self.resolved.len())
            .finish_non_exhaustive()
    }
}

impl ServerProposalQueue {
    fn next_ticket(&mut self) -> ServerProposalTicket {
        let ticket = ServerProposalTicket(self.next_id);
        self.next_id = self.next_id.checked_add(1).expect("proposal id exhausted");
        ticket
    }

    /// Stage an engine-owned proposal for the next adjudication pass.
    pub fn stage(&mut self, action: ServerProposalAction) -> ServerProposalTicket {
        let ticket = self.next_ticket();
        self.staged.push(Pending { id: ticket.0, action, reply: None });
        ticket
    }

    /// Drains all resolutions from the just-completed adjudication pass.
    pub fn take_resolutions(&mut self) -> Vec<ServerProposalResolution> {
        std::mem::take(&mut self.resolved)
    }
}

/// Installs the proposal message and bounded ingress around the plugin-owned
/// `TickSet::Adjudicate` pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct ServerProposalPlugin;

impl bevy_app::Plugin for ServerProposalPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        app.insert_resource(ServerProposalQueue {
            receiver: Mutex::new(receiver),
            next_id: 0,
            staged: Vec::new(),
            pending: Vec::new(),
            resolved: Vec::new(),
        });
        app.insert_resource(ServerProposalHandle { sender });
        app.init_resource::<ServerProposalDecisions>();
        app.init_resource::<PaperEventBus>();
        app.add_message::<ServerProposal>();
        app.add_systems(super::GameTick, drain_proposals.in_set(TickSet::Drain));
        app.add_systems(
            super::GameTick,
            dispatch_paper_events.in_set(TickSet::Adjudicate),
        );
        app.add_systems(super::GameTick, apply_proposals.in_set(TickSet::Apply));
    }
}

fn dispatch_paper_events(
    mut proposals: MessageReader<ServerProposal>,
    mut events: ResMut<PaperEventBus>,
    mut decisions: ResMut<ServerProposalDecisions>,
) {
    for proposal in proposals.read() {
        if let Some(verdict) = events.adjudicate(&proposal.action) {
            // The bus has already applied Paper's priority/order rules. Make
            // its final verdict authoritative over native proposal consumers.
            decisions.decide(proposal.id(), i32::MIN, verdict);
        }
    }
}

fn drain_proposals(mut inbox: ResMut<ServerProposalQueue>, mut writer: MessageWriter<ServerProposal>) {
    loop {
        let request = match inbox
            .receiver
            .lock()
            .expect("proposal inbox lock poisoned")
            .try_recv()
        {
            Ok(request) => request,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        };
        let id = inbox.next_ticket().0;
        writer.write(ServerProposal {
            id,
            action: request.action.clone(),
        });
        inbox.pending.push(Pending {
            id,
            action: request.action,
            reply: Some(request.reply),
        });
    }
    for pending in std::mem::take(&mut inbox.staged) {
        writer.write(ServerProposal { id: pending.id, action: pending.action.clone() });
        inbox.pending.push(pending);
    }
}

fn apply_proposals(mut inbox: ResMut<ServerProposalQueue>, mut decisions: ResMut<ServerProposalDecisions>) {
    for pending in std::mem::take(&mut inbox.pending) {
        let outcome = match decisions.take(pending.id) {
            ProposalVerdict::Allow => Ok(pending.action),
            ProposalVerdict::Deny => Err(ProposalRefusal::Denied),
            ProposalVerdict::Replace(action) => Ok(action),
        };
        if let Some(reply) = pending.reply {
            let _ = reply.send(outcome.clone());
        }
        inbox.resolved.push(ServerProposalResolution {
            ticket: ServerProposalTicket(pending.id),
            outcome,
        });
    }
    // A plugin added after a request was applied can still observe the aged
    // message on its first tick. Its decision cannot affect a completed
    // request, and retaining it would make the decision table grow forever.
    decisions.decisions.clear();
}
