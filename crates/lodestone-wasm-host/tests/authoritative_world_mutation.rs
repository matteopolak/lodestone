//! Cross-tier authority gate for the world/block plugin surface.
//!
//! The guest is built as a separate `wasm32` crate and driven through the real
//! conductor. Its copied request is then handed to a running integrated server,
//! whose native adjudicator replaces the requested state before the source write.
//! This closes the gap between a request-queue unit and the actual authoritative
//! world mutation without giving the guest a world guard or source handle.

mod support;

use bevy_app::{App, Plugin};
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::ResMut;
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_core::State;
use lodestone_data::block_states::StateId;
use lodestone_ecs::GameTick;
use lodestone_model::BlockPos;
use lodestone_server::ecs::{
    GameTick as ServerGameTick, ProposalVerdict, ServerApp, ServerProposal,
    ServerProposalAction, ServerProposalDecisions, TickSet,
};
use lodestone_server::{
    BlockMutationRefusal, ChunkColumn, ChunkSource, IntegratedServer, ServerBound,
    ServerDirective, ServerProtocol,
};
use lodestone_wasm_host::{
    Capability, CapabilitySet, PendingWasmWorldMutations, PluginHost, WasmHostPlugin,
};
use uuid::Uuid;

const MIN_Y: i32 = 0;
const HEIGHT: i32 = 16;

#[derive(Debug, Default)]
struct FlatWorld {
    state: std::sync::Mutex<Option<(BlockPos, String)>>,
}

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(MIN_Y, HEIGHT)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let state = self.state.lock().expect("test world lock");
        state
            .as_ref()
            .filter(|(pos, _)| pos.x == x && pos.y == y && pos.z == z)
            .map_or_else(|| "minecraft:air".to_owned(), |(_, value)| value.clone())
    }

    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<StateId> {
        (MIN_Y..MIN_Y + HEIGHT)
            .contains(&y)
            .then(|| self.block_state(x, y, z))
            .and_then(|state| StateId::from_state_str(&state))
    }

    fn resident_column(&self, _cx: i32, _cz: i32) -> Option<ChunkColumn> {
        Some(ChunkColumn::new(MIN_Y, HEIGHT))
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        *self.state.lock().expect("test world lock") =
            Some((BlockPos::new(x, y, z), name.to_owned()));
    }
}

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

/// The native consumer is intentionally authoritative: it replaces the guest's
/// state, then the server applies that replacement to its retained source.
struct NativeReplacement;

impl Plugin for NativeReplacement {
    fn build(&self, app: &mut App) {
        app.add_systems(ServerGameTick, replace_block.in_set(TickSet::Adjudicate));
    }
}

fn replace_block(
    mut proposals: MessageReader<ServerProposal>,
    mut decisions: ResMut<ServerProposalDecisions>,
) {
    for proposal in proposals.read() {
        if let ServerProposalAction::SetResidentBlock { pos, .. } = &proposal.action {
            if *pos == BlockPos::new(2, 4, 3) {
                decisions.decide(
                    proposal.id(),
                    0,
                    ProposalVerdict::Replace(ServerProposalAction::SetResidentBlock {
                        pos: *pos,
                        state: StateId::from_state_str("minecraft:diamond_block")
                            .expect("diamond block is in the generated state table"),
                    }),
                );
            }
        }
    }
}

fn world_write_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::WriteWorld])
}

fn world_write_host_policy() -> CapabilitySet {
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::WriteWorld);
    policy
}

#[tokio::test]
async fn wasm_request_reaches_native_adjudicator_and_authoritative_source() {
    let wasm = support::build_example_plugin(&["world-write"]);
    let mut host = PluginHost::new(world_write_host_policy()).expect("wasm engine");
    host.load_file("world-write", &wasm, &world_write_capabilities())
        .expect("the separately built guest must load with its explicit grant");

    let mut client = lodestone_app::client_app();
    client.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(
        &mut client,
        lodestone_physics::PlayerState::at(lodestone_physics::Vec3d::new(0.5, 1.0, 0.5), 0.0),
    );
    client.world_mut().run_schedule(GameTick);

    let requests = client
        .world_mut()
        .resource_mut::<PendingWasmWorldMutations>()
        .take_requests();
    assert_eq!(requests.len(), 1, "the guest must produce exactly one copied request");
    let (plugin, request) = requests.into_iter().next().expect("request");
    assert_eq!(plugin, 0, "the copied result route must preserve guest identity");

    let server_app = ServerApp::bootstrap_with(|app| {
        app.add_plugins(NativeReplacement);
    });
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        SilentProtocol,
        FlatWorld::default(),
        (0..=0, 0..=0),
        (0, 0),
        0,
        1,
        server_app,
    );
    std::mem::forget(client_io);

    let state = StateId::new(request.state_id).expect("guest state id must pass the server boundary");
    server
        .set_resident_block_state_proposed(
            BlockPos::new(request.pos.x, request.pos.y, request.pos.z),
            state,
        )
        .await
        .expect("native adjudication must allow the replacement");
    assert_eq!(
        server.resident_block_state_id(2, 4, 3),
        Some(StateId::from_state_str("minecraft:diamond_block").expect("state table")),
        "the native replacement must be the state retained by the authoritative source"
    );
    assert_eq!(
        server
            .set_resident_block_state_proposed(BlockPos::new(2, HEIGHT, 3), state)
            .await,
        Err(BlockMutationRefusal::OutOfBounds),
        "the server must preserve its finite validation outcome for an invalid height"
    );

    server.shutdown().await;
}
