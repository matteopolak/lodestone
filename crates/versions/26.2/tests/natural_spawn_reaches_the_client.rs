//! Stationary integrated-server spawning through protocol 776 into shared ECS.
//!
//! The injected control has independently chosen identity and coordinates. Raw
//! packet/type ordinals below come from the 26.2 generator's packet and registry
//! reports. The flat fixture isolates natural spawning from terrain generation;
//! this gate does not establish browser transport, ordinary-world eligibility,
//! draw extraction, or pixels.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bevy_ecs::message::{MessageReader, Messages};
use bevy_ecs::prelude::{Res, Resource};
use bevy_ecs::schedule::IntoScheduleConfigs;
use lodestone_client::{ClientBuilder, ClientHandle, EventStream, LoginProfile, PlayerLoadedPolicy, ServerAddress};
use lodestone_core::Reader;
use lodestone_data::block::Block;
use lodestone_data::block_states::StateId;
use lodestone_ecs::entity::{EntityIndex, EntityKind, EntityUuid, Position};
use lodestone_ecs::{EcsHandle, OutboundRawPacket, OutboundRawPacketBus, RawPacket, RawPacketBus, RawPacketLimits};
use lodestone_model::{ClientAction, ClientEvent, ConnectionState, Rotation, Vec3};
use lodestone_server::dimension::Dimension;
use lodestone_server::ecs::{GameTick, ServerApp, ServerProposal, ServerProposalAction, TickSet};
use lodestone_server::{ChunkColumn, ChunkSource, EntitySnapshot, EntitySource, IntegratedServer};
use lodestone_v26_2::{V770ServerProtocol, adapter};
use uuid::Uuid;

const FLOOR: i32 = 70;
const TICK_BOUND: u64 = 400;
const WALL_BOUND: Duration = Duration::from_secs(40);
// External 26.2 packet report: Play clientbound AddEntity = 1;
// Play serverbound movement = 30..=33 and PlayerLoaded = 44.
const ADD_ENTITY: i32 = 1;
const PLAYER_LOADED: i32 = 44;
// External 26.2 entity registry report, independently of the encoder/adapter.
const PLAINS_TYPES: &[(&str, i32)] = &[
    ("minecraft:chicken", 26), ("minecraft:cow", 30),
    ("minecraft:donkey", 36), ("minecraft:horse", 66),
    ("minecraft:pig", 100), ("minecraft:sheep", 111),
];

#[derive(Clone)]
struct Plains(ChunkColumn);

impl Plains {
    fn new() -> Self {
        let mut column = ChunkColumn::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                for y in -64..FLOOR {
                    column.set_block_id(x, y, z, Block::Stone.default_state());
                }
                column.set_block_id(x, FLOOR, z, Block::GrassBlock.default_state());
            }
        }
        column.prime_client_heightmaps();
        assert_eq!(column.generation_stage(), lodestone_server::ChunkGenerationStage::Full);
        assert_eq!(column.biome_state_at(0, FLOOR + 1, 0), "minecraft:plains");
        Self(column)
    }
}

impl ChunkSource for Plains {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn { self.0.clone() }
    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.0.block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String { "minecraft:plains".into() }
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}

struct Injected(EntitySnapshot);
impl EntitySource for Injected {
    fn snapshots(&self) -> Vec<EntitySnapshot> { vec![self.0.clone()] }
}

#[derive(Debug, Clone)]
struct WireSpawn {
    id: i32,
    uuid: Uuid,
    ordinal: i32,
    position: Vec3,
}

#[derive(Default)]
struct Witness {
    wire: HashMap<i32, WireSpawn>,
    decoded: HashMap<i32, (String, Vec3)>,
    outbound_movement: usize,
    loaded: usize,
}

impl Witness {
    fn collect(&mut self, ecs: &EcsHandle, events: &mut EventStream) {
        while let Ok(event) = events.try_recv() {
            if let ClientEvent::EntitySpawned { entity_id, entity_type, pos, .. } = event {
                self.decoded.insert(entity_id, (entity_type.to_string(), pos));
            }
        }
        let mut world = ecs.write();
        assert_eq!(world.resource::<RawPacketBus>().stats().dropped_packets, 0,
            "raw inbound detector must not silently discard packets");
        assert_eq!(world.resource::<OutboundRawPacketBus>().stats().dropped_packets, 0,
            "outbound movement detector must not silently discard packets");
        for packet in world.resource_mut::<Messages<RawPacket>>().drain() {
            if packet.state == ConnectionState::Play && packet.packet_id == ADD_ENTITY {
                assert_eq!(packet.protocol, 776);
                let mut reader = Reader::new(&packet.payload);
                let spawn = WireSpawn {
                    id: reader.var_i32().expect("wire entity id"),
                    uuid: reader.uuid().expect("wire UUID"),
                    ordinal: reader.var_i32().expect("wire species ordinal"),
                    position: Vec3::new(reader.f64().unwrap(), reader.f64().unwrap(), reader.f64().unwrap()),
                };
                self.wire.insert(spawn.id, spawn);
            }
        }
        for packet in world.resource_mut::<Messages<OutboundRawPacket>>().drain() {
            if packet.state == ConnectionState::Play {
                self.outbound_movement += usize::from((30..=33).contains(&packet.packet_id));
                self.loaded += usize::from(packet.packet_id == PLAYER_LOADED);
            }
        }
        // These are observation windows, independent of the server simulation.
        world.insert_resource(raw_bus());
        world.insert_resource(OutboundRawPacketBus::default());
    }
}

fn raw_bus() -> RawPacketBus {
    RawPacketBus::with_limits(RawPacketLimits {
        max_packets_per_tick: 4096,
        max_payload_bytes_per_tick: 16 * 1024 * 1024,
    })
}

fn connect(io: tokio::io::DuplexStream) -> (ClientHandle, EventStream, EcsHandle) {
    let ecs = lodestone_ecs::new_ingest_handle();
    let session = {
        let mut world = ecs.write();
        world.insert_resource(raw_bus());
        world.insert_resource(OutboundRawPacketBus::default());
        world.insert_resource(Messages::<RawPacket>::default());
        world.insert_resource(Messages::<OutboundRawPacket>::default());
        lodestone_ecs::spawn_session(&mut world)
    };
    let (handle, events) = ClientBuilder::new(
        ServerAddress { host: "memory".into(), port: 0 },
        LoginProfile { username: "Stationary".into(), uuid: Uuid::from_u128(0x5350_4157) },
        Box::new(adapter()),
    )
        .ecs(ecs.clone(), session)
        .player_loaded_policy(PlayerLoadedPolicy::Manual)
        .connect_with(io);
    (handle, events, ecs)
}

async fn poll(witness: &mut Witness, ecs: &EcsHandle, events: &mut EventStream) {
    tokio::time::sleep(Duration::from_millis(20)).await;
    witness.collect(ecs, events);
}

fn indexed(ecs: &EcsHandle, id: i32) -> bool {
    ecs.read().resource::<EntityIndex>().get(id).is_some()
}

fn assert_ecs(ecs: &EcsHandle, wire: &WireSpawn, species: &str) -> Vec3 {
    let world = ecs.read();
    let entity = world.resource::<EntityIndex>().get(wire.id).expect("wire ID in shared ECS");
    assert_eq!(world.get::<EntityKind>(entity).unwrap().0.to_string(), species);
    assert_eq!(world.get::<EntityUuid>(entity).unwrap().0, wire.uuid);
    world.get::<Position>(entity).expect("render-consumed Position").0
}

async fn injected_detector_control() {
    let expected = EntitySnapshot {
        id: 91_003,
        uuid: Uuid::from_u128(0x1234_5678_9abc_def0),
        entity_type: "minecraft:cow".parse().unwrap(),
        position: Vec3::new(11.25, 71.0, -6.75),
        rotation: Rotation::new(0.0, 0.0),
        head_yaw: 0.0,
        velocity: Vec3::new(0.0, 0.0, 0.0),
        on_ground: true,
        metadata: Vec::new(),
        object_data: 0,
        leash_link: None,
    };
    let (server, io) = IntegratedServer::open_in_memory_with_entities(
        V770ServerProtocol, Plains::new(), Injected(expected.clone()), 1,
    );
    let (handle, mut events, ecs) = connect(io);
    let mut witness = Witness::default();
    let deadline = tokio::time::Instant::now() + WALL_BOUND;
    while !(witness.decoded.contains_key(&expected.id) && indexed(&ecs, expected.id)) {
        assert!(tokio::time::Instant::now() < deadline, "injected detector failed: {:?}", witness.wire);
        poll(&mut witness, &ecs, &mut events).await;
    }
    let wire = witness.wire.get(&expected.id).expect("received AddEntity bytes");
    assert_eq!(wire.ordinal, 30, "external cow registry ordinal");
    assert_eq!(wire.uuid, expected.uuid);
    assert_eq!(wire.position, expected.position);
    assert_eq!(witness.decoded[&expected.id], ("minecraft:cow".into(), expected.position));
    assert_eq!(assert_ecs(&ecs, wire, "minecraft:cow"), expected.position);
    assert!(witness.outbound_movement <= 1, "only the normal placement echo is permitted");
    eprintln!("injected detector: id={} type=30 minecraft:cow position={:?}, wire+event+ECS", wire.id, wire.position);
    drop(handle);
    server.shutdown().await;
}

#[derive(Resource, Clone, Default)]
struct Planned(Arc<Mutex<Vec<(String, Vec3)>>>);

fn observe_planned(mut proposals: MessageReader<ServerProposal>, planned: Res<Planned>) {
    for proposal in proposals.read() {
        if let ServerProposalAction::NaturalSpawnMob { entity_type, pos, .. } = &proposal.action {
            planned.0.lock().unwrap().push((entity_type.to_string(), *pos));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stationary_natural_spawning_crosses_real_wire_and_shared_ecs() {
    injected_detector_control().await;
    let planned = Planned::default();
    let server_app = ServerApp::bootstrap_with(|app| {
        app.insert_resource(planned.clone());
        app.add_systems(GameTick, observe_planned.in_set(TickSet::Adjudicate));
    });
    let (server, io) = IntegratedServer::open_in_memory_with_mobs_and_server_app(
        V770ServerProtocol, Plains::new(), (-3..=3, -3..=3), (8, 8), 3, server_app,
    );
    for (rule, value) in [
        ("spawn_mobs", "false"), ("random_tick_speed", "0"),
        ("spawn_patrols", "false"), ("spawn_wandering_traders", "false"),
        ("spawn_phantoms", "false"), ("advance_time", "false"), ("advance_weather", "false"),
    ] {
        server.world_state().set_rule(rule, value).unwrap();
    }
    let runtime = server.world_state().dimension_runtime(Dimension::Overworld).unwrap();
    let connected_at = tokio::time::Instant::now();
    let (handle, mut events, ecs) = connect(io);
    let mut witness = Witness::default();
    let deadline = tokio::time::Instant::now() + WALL_BOUND;
    while handle.position().is_none() || handle.loaded_chunk_count() < 49 {
        assert!(tokio::time::Instant::now() < deadline, "stationary join failed; chunks={} position={:?}",
            handle.loaded_chunk_count(), handle.position());
        poll(&mut witness, &ecs, &mut events).await;
    }
    let position = handle.position().unwrap();
    let presence = server.world_state().player_registry().perceptions(Dimension::Overworld);
    assert_eq!(presence.len(), 1, "Play join must register without movement");
    assert_eq!(presence[0].perception.position, position);
    for _ in 0..10 { poll(&mut witness, &ecs, &mut events).await; }
    assert_eq!(witness.loaded, 0, "manual policy withholds readiness");
    assert_eq!(witness.outbound_movement, 1, "the normal join placement echo must be observed");
    // A client that never reports loaded holds the world only for the
    // server's load timeout (3 s of vitals ticks from join), then the world
    // runs anyway. So the hold is checked against that clock: no tick before
    // the timeout can have elapsed, and afterwards no more ticks than the
    // 20 Hz cadence allows since it did.
    let held_for = connected_at.elapsed();
    let ticks = server.tick_stats().unwrap().tick_count;
    let timeout = Duration::from_secs(3);
    let allowed = held_for.saturating_sub(timeout).as_millis() as u64 / 50 + 2;
    assert!(ticks <= allowed,
        "client hold must stop simulation: {ticks} ticks after {held_for:?}, at most {allowed} allowed");
    if held_for < timeout {
        assert_eq!(ticks, 0, "inside the load timeout the world must not tick at all");
    }
    handle.send_action(ClientAction::PlayerLoaded).unwrap();

    while server.tick_stats().unwrap().tick_count < 20 || witness.loaded < 1 {
        assert!(tokio::time::Instant::now() < deadline, "client and seed holds never released");
        poll(&mut witness, &ecs, &mut events).await;
    }
    assert_eq!(witness.loaded, 1);
    assert!(witness.wire.is_empty() && witness.decoded.is_empty(), "gate-off control must deliver no entities");
    assert_eq!(ecs.read().resource::<EntityIndex>().len(), 1, "gate-off ECS contains only the local player");
    assert!(runtime.entities().snapshots().is_empty());
    assert!(planned.0.lock().unwrap().is_empty());
    for cz in -3..=3 {
        for cx in -3..=3 {
            assert_eq!(server.resident_block_state_id(cx * 16, FLOOR, cz * 16), Some(Block::GrassBlock.default_state()),
                "spawn support must be actually resident at chunk ({cx},{cz})");
        }
    }
    let start_tick = server.tick_stats().unwrap().tick_count;
    server.world_state().set_rule("spawn_mobs", "true").unwrap();
    let mut accepted = None;
    loop {
        poll(&mut witness, &ecs, &mut events).await;
        let published = runtime.entities().snapshots();
        for snapshot in &published {
            if let (Some(wire), Some(decoded)) = (witness.wire.get(&snapshot.id), witness.decoded.get(&snapshot.id)) {
                if indexed(&ecs, snapshot.id) {
                    accepted = Some((snapshot.clone(), wire.clone(), decoded.clone()));
                    break;
                }
            }
        }
        if accepted.is_some() { break; }
        let elapsed = server.tick_stats().unwrap().tick_count - start_tick;
        assert!(elapsed < TICK_BOUND && tokio::time::Instant::now() < deadline,
            "natural acceptance failed: ticks={elapsed}, planned={}, published={}, wire={}, decoded={}, stationary={position:?}",
            planned.0.lock().unwrap().len(), published.len(), witness.wire.len(), witness.decoded.len());
    }
    let (published, wire, decoded) = accepted.unwrap();
    let species = published.entity_type.to_string();
    let expected_ordinal = PLAINS_TYPES.iter().find(|(name, _)| *name == species).expect("independent plains creature species").1;
    assert_eq!(wire.ordinal, expected_ordinal);
    assert_eq!(published.uuid, wire.uuid);
    assert_eq!(decoded, (species.clone(), wire.position));
    assert!(planned.0.lock().unwrap().iter().any(|(kind, pos)| kind == &species && *pos == wire.position),
        "first AddEntity must preserve an actually proposed natural species and position: {wire:?}");
    assert_eq!(wire.position.y, 71.0, "independent grass-top geometry");
    let ecs_position = assert_ecs(&ecs, &wire, &species);
    assert_eq!(ecs_position.y, 71.0);
    assert_eq!(handle.position(), Some(position), "the player remains stationary");
    assert_eq!(witness.outbound_movement, 1, "no further movement packet may drive the entity streamer");
    let ticks = server.tick_stats().unwrap().tick_count - start_tick;
    eprintln!("natural acceptance: ticks={ticks} resident_support=49 proposals={} publication={} AddEntity={} decoded={} id={} ordinal={} species={species} wire_position={:?} ecs_position={ecs_position:?}",
        planned.0.lock().unwrap().len(), runtime.entities().snapshots().len(), witness.wire.len(), witness.decoded.len(), wire.id, wire.ordinal, wire.position);
    drop(handle);
    server.shutdown().await;
}
