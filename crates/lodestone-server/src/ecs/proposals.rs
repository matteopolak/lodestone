//! Native-plugin adjudication of server actions on `TickSet::Adjudicate`.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Mutex;
use std::time::Duration;

use bevy_ecs::message::{Message, MessageWriter};
use bevy_ecs::prelude::{ResMut, Resource};
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_model::{ResourceKey, Vec3};

use super::TickSet;

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
}

/// A plugin's answer to a [`ServerProposal`].
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalVerdict {
    /// Leave the proposed action unchanged.
    Allow,
    /// Refuse the action; the caller receives [`SpawnProposalRefusal::Denied`].
    Deny,
    /// Apply a replacement action instead of the original one.
    Replace(ServerProposalAction),
}

/// Why an externally proposed spawn did not become a mob.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpawnProposalRefusal {
    /// A plugin denied the proposal.
    Denied,
    /// The tick task did not consume the request within the bounded wait.
    TimedOut,
    /// The bounded ingress queue is full or the tick task has stopped.
    Unavailable,
}

struct Request {
    action: ServerProposalAction,
    reply: tokio::sync::oneshot::Sender<Result<ServerProposalAction, SpawnProposalRefusal>>,
}

/// Cloneable ingress for callers outside the tick-owned ECS world.
#[derive(Clone, Debug, Resource)]
pub struct ServerProposalHandle {
    sender: SyncSender<Request>,
}

impl ServerProposalHandle {
    /// Submit a spawn and await one `Drain → Adjudicate → Apply` pass.
    pub async fn spawn_mob(
        &self,
        entity_type: ResourceKey,
        pos: Vec3,
    ) -> Result<ServerProposalAction, SpawnProposalRefusal> {
        let (reply, receiver) = tokio::sync::oneshot::channel();
        match self.sender.try_send(Request {
            action: ServerProposalAction::SpawnMob { entity_type, pos },
            reply,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                return Err(SpawnProposalRefusal::Unavailable);
            }
        }
        tokio::time::timeout(RESPONSE_TIMEOUT, receiver)
            .await
            .map_or(Err(SpawnProposalRefusal::TimedOut), |outcome| {
                outcome.unwrap_or(Err(SpawnProposalRefusal::Unavailable))
            })
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
    reply: tokio::sync::oneshot::Sender<Result<ServerProposalAction, SpawnProposalRefusal>>,
}

#[derive(Resource)]
struct ProposalInbox {
    receiver: Mutex<Receiver<Request>>,
    next_id: u64,
    pending: Vec<Pending>,
}

/// Installs the proposal message and bounded ingress around the plugin-owned
/// `TickSet::Adjudicate` pass.
#[derive(Debug, Default, Clone, Copy)]
pub struct ServerProposalPlugin;

impl bevy_app::Plugin for ServerProposalPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        app.insert_resource(ProposalInbox {
            receiver: Mutex::new(receiver),
            next_id: 0,
            pending: Vec::new(),
        });
        app.insert_resource(ServerProposalHandle { sender });
        app.init_resource::<ServerProposalDecisions>();
        app.add_message::<ServerProposal>();
        app.add_systems(super::GameTick, drain_proposals.in_set(TickSet::Drain));
        app.add_systems(super::GameTick, apply_proposals.in_set(TickSet::Apply));
    }
}

fn drain_proposals(mut inbox: ResMut<ProposalInbox>, mut writer: MessageWriter<ServerProposal>) {
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
        let id = inbox.next_id;
        inbox.next_id = inbox.next_id.checked_add(1).expect("proposal id exhausted");
        writer.write(ServerProposal {
            id,
            action: request.action.clone(),
        });
        inbox.pending.push(Pending {
            id,
            action: request.action,
            reply: request.reply,
        });
    }
}

fn apply_proposals(mut inbox: ResMut<ProposalInbox>, mut decisions: ResMut<ServerProposalDecisions>) {
    for pending in std::mem::take(&mut inbox.pending) {
        let outcome = match decisions.take(pending.id) {
            ProposalVerdict::Allow => Ok(pending.action),
            ProposalVerdict::Deny => Err(SpawnProposalRefusal::Denied),
            ProposalVerdict::Replace(action) => Ok(action),
        };
        let _ = pending.reply.send(outcome);
    }
    // A plugin added after a request was applied can still observe the aged
    // message on its first tick. Its decision cannot affect a completed
    // request, and retaining it would make the decision table grow forever.
    decisions.decisions.clear();
}
