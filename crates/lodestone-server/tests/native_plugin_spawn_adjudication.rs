//! A running native-plugin server adjudicates public mob-spawn proposals before
//! touching the live `MobHandle`.

use bevy_app::{App, Plugin};
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::ResMut;
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_core::State;
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, ResourceKey, Vec3};
use lodestone_server::ecs::{
    GameTick, ProposalVerdict, ServerApp, ServerProposal, ServerProposalAction,
    ServerProposalDecisions, SpawnProposalRefusal, TickSet,
};
use lodestone_server::{
    BlockMutationRefusal, ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective,
    ServerProtocol,
};
use std::collections::HashMap;
use std::sync::Mutex;
use uuid::Uuid;

const MIN_Y: i32 = 0;
const HEIGHT: i32 = 16;
const REPLACEMENT_POS: Vec3 = Vec3::new(7.25, 9.5, -3.75);

#[derive(Debug)]
struct SilentProtocol;

impl ServerProtocol for SilentProtocol {
    fn decode(&self, _state: State, _packet_id: i32, _payload: &[u8]) -> ServerBound {
        ServerBound::Ignored
    }

    fn login_success(&self, _username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::None
    }

    fn encode_chunk(&self, _cx: i32, _cz: i32, _column: &ChunkColumn) -> ServerDirective {
        ServerDirective::None
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
}

#[derive(Debug, Default)]
struct FlatWorld {
    blocks: Mutex<HashMap<(i32, i32, i32), String>>,
}

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(MIN_Y, HEIGHT)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.blocks
            .lock()
            .expect("test world lock")
            .get(&(x, y, z))
            .cloned()
            .unwrap_or_else(|| "minecraft:air".to_string())
    }

    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        (MIN_Y..MIN_Y + HEIGHT).contains(&y)
            .then(|| self.block_state(x, y, z))
            .and_then(|state| StateId::from_state_str(&state))
    }

    fn resident_column(&self, _cx: i32, _cz: i32) -> Option<ChunkColumn> {
        Some(ChunkColumn::new(MIN_Y, HEIGHT))
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.blocks
            .lock()
            .expect("test world lock")
            .insert((x, y, z), name.to_string());
    }
}

struct SpawnPolicy;

impl Plugin for SpawnPolicy {
    fn build(&self, app: &mut App) {
        // The deny system runs first. The later replacement has the stronger
        // priority, so a first-writer-wins implementation would fail this gate.
        app.add_systems(
            GameTick,
            (deny_spawns, replace_pigs).chain().in_set(TickSet::Adjudicate),
        );
    }
}

struct BlockPolicy;

impl Plugin for BlockPolicy {
    fn build(&self, app: &mut App) {
        app.add_systems(GameTick, decide_blocks.in_set(TickSet::Adjudicate));
    }
}

fn decide_blocks(
    mut proposals: MessageReader<ServerProposal>,
    mut decisions: ResMut<ServerProposalDecisions>,
) {
    for proposal in proposals.read() {
        let ServerProposalAction::SetResidentBlock { pos, .. } = &proposal.action else {
            continue;
        };
        if pos.x == 1 {
            decisions.decide(proposal.id(), 0, ProposalVerdict::Deny);
        } else if pos.x == 2 {
            decisions.decide(
                proposal.id(),
                0,
                ProposalVerdict::Replace(ServerProposalAction::SetResidentBlock {
                    pos: *pos,
                    state: state("minecraft:diamond_block"),
                }),
            );
        }
    }
}

fn deny_spawns(
    mut proposals: MessageReader<ServerProposal>,
    mut decisions: ResMut<ServerProposalDecisions>,
) {
    for proposal in proposals.read() {
        if matches!(proposal.action, ServerProposalAction::SpawnMob { .. }) {
            decisions.decide(proposal.id(), 20, ProposalVerdict::Deny);
        }
    }
}

fn replace_pigs(
    mut proposals: MessageReader<ServerProposal>,
    mut decisions: ResMut<ServerProposalDecisions>,
) {
    for proposal in proposals.read() {
        let ServerProposalAction::SpawnMob { entity_type, .. } = &proposal.action else {
            continue;
        };
        if entity_type == &key("minecraft:pig") {
            decisions.decide(
                proposal.id(),
                5,
                ProposalVerdict::Replace(ServerProposalAction::SpawnMob {
                    entity_type: key("minecraft:cow"),
                    pos: REPLACEMENT_POS,
                }),
            );
        }
    }
}

fn key(value: &str) -> ResourceKey {
    value.parse().expect("test resource key")
}

fn state(value: &str) -> StateId {
    StateId::from_state_str(value).expect("test block state exists")
}

#[tokio::test]
async fn native_plugins_deny_or_prioritize_replacement_before_integrated_spawn() {
    let server_app = ServerApp::bootstrap_with(|app| {
        app.add_plugins(SpawnPolicy);
    });
    let (server, client) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        SilentProtocol,
        FlatWorld::default(),
        (0..=0, 0..=0),
        (0, 0),
        0,
        1,
        server_app,
    );
    std::mem::forget(client);
    let mobs = server.mobs().expect("the primary tick task owns a live mob simulation");

    let denied = server
        .spawn_mob_proposed(key("minecraft:cow"), Vec3::new(1.0, 2.0, 3.0))
        .await;
    assert_eq!(denied, Err(SpawnProposalRefusal::Denied));
    assert!(mobs.with(|sim| sim.snapshots()).is_empty(), "a denied proposal must not mutate the sim");

    let id = server
        .spawn_mob_proposed(key("minecraft:pig"), Vec3::new(-1.0, 0.0, 4.0))
        .await
        .expect("the stronger replacement verdict must allow the spawn");
    let spawned = mobs.with(|sim| {
        sim.snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id == id)
    });
    let spawned = spawned.expect("the resolved proposal must reach the live simulation");
    assert_eq!(spawned.entity_type, key("minecraft:cow"));
    assert_eq!(spawned.position, REPLACEMENT_POS);

    assert_eq!(
        server.despawn_mob_proposed(id).await,
        Ok(true),
        "the same queue must apply a checked despawn after its plugins allow it"
    );
    assert!(
        mobs.with(|sim| sim.get(id).is_none()),
        "a checked despawn must remove the resolved id from the live simulation"
    );

    server.shutdown().await;
}

/// The native server consumer and the shared proposal queue must reach the
/// retained source, not merely resolve a verdict in an isolated ECS app.
#[tokio::test]
async fn native_plugin_block_mutations_are_adjudicated_then_reach_the_authoritative_source() {
    let server_app = ServerApp::bootstrap_with(|app| {
        app.add_plugins(BlockPolicy);
    });
    let (server, client) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        SilentProtocol,
        FlatWorld::default(),
        (0..=0, 0..=0),
        (0, 0),
        0,
        1,
        server_app,
    );
    std::mem::forget(client);

    let denied = server
        .set_resident_block_state_proposed(BlockPos::new(1, 4, 3), state("minecraft:gold_block"))
        .await;
    assert_eq!(denied, Err(BlockMutationRefusal::Denied));
    assert_eq!(
        server.resident_block_state_id(1, 4, 3),
        Some(state("minecraft:air")),
        "a denied proposal must leave the authoritative source unchanged"
    );

    server
        .set_resident_block_state_proposed(BlockPos::new(2, 4, 3), state("minecraft:gold_block"))
        .await
        .expect("the replacement proposal must write through the live source");
    assert_eq!(
        server.resident_block_state_id(2, 4, 3),
        Some(state("minecraft:diamond_block")),
        "the resolved replacement, not the caller's requested state, must become authoritative"
    );

    assert_eq!(
        server
            .set_resident_block_state_proposed(BlockPos::new(2, HEIGHT, 3), state("minecraft:gold_block"))
            .await,
        Err(BlockMutationRefusal::OutOfBounds),
        "a finite validation result must replace a source-specific error string"
    );

    server.shutdown().await;
}
