//! Bounded packet replay against the public client read-model.
//!
//! A scripted adapter loads one chunk and accepts block, entity, inventory, and
//! generic-container packets, while independent maps apply the same actions.
//! Fixed scripts cover each read-model dimension, and deterministic generated
//! campaigns exercise 464 packet replays. The comparison reads client-owned
//! state after each tick, proving that packet handling reaches the read model
//! rather than merely producing an event. A separate fixed proof starts an
//! `IntegratedServer` and synchronizes against its real tick counter while
//! reading the retained `ChunkSource` directly.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use lodestone_client::{
    BlockPos, ClientBuilder, ClientEvent, ConnectionState, Directive, EventStream, LoginProfile,
    Rotation, ServerAddress, Vec3, VersionAdapter,
};
use lodestone_core::State;
use lodestone_fuzz::differential::{
    Action, DifferentialOutcome, Script, ScriptStep, WorldOracle, run_differential,
};
use lodestone_model::{
    AdapterError, ChunkPos as ModelChunkPos, ContainerStateId, EntityMovement, Identifier,
    ItemComponents, ItemStack, SectionPos, Text, WorldSink,
};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    ChunkColumn as ServerChunkColumn, ChunkSource, IntegratedServer, ServerBound,
    ServerDirective, ServerProtocol,
};
use lodestone_world::{
    ChunkColumn, ChunkPos as WorldChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind,
};
use tokio::io::DuplexStream;
use tokio::runtime::{Builder, Runtime};
use uuid::Uuid;

const LOAD_PACKET: i32 = 1;
const BLOCK_UPDATE_PACKET: i32 = 2;
const ENTITY_SPAWN_PACKET: i32 = 3;
const ENTITY_MOVE_PACKET: i32 = 4;
const ENTITY_REMOVE_PACKET: i32 = 5;
const INVENTORY_CONTENT_PACKET: i32 = 6;
const INVENTORY_SLOT_PACKET: i32 = 7;
const CONTAINER_OPEN_PACKET: i32 = 8;
const CONTAINER_CONTENT_PACKET: i32 = 9;
const CONTAINER_SLOT_PACKET: i32 = 10;
const TARGET: (i32, i32, i32) = (1, 0, 1);
const INVENTORY_TARGET_SLOT: usize = 36;
const CONTAINER_WINDOW_ID: i32 = 4;
const CONTAINER_TARGET_SLOT: usize = 0;
const AIR: &str = "minecraft:air";
const STONE: &str = "minecraft:stone";
const AIR_ID: u32 = 0;
const STONE_ID: u32 = 1;

#[derive(Debug, Default)]
struct ScriptedAdapter;

impl VersionAdapter for ScriptedAdapter {
    fn protocol_version(&self) -> i32 {
        0
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        &["differential-fixture"]
    }

    fn supports(&self, _protocol: i32) -> bool {
        true
    }

    fn begin_login(
        &self,
        _profile: &LoginProfile,
        _server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        Ok(vec![Directive::SetState(ConnectionState::Play)])
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        if state != ConnectionState::Play {
            return Err(AdapterError::UnsupportedPacketState { state });
        }
        match packet_id {
            LOAD_PACKET => {
                if !payload.is_empty() {
                    return Err(AdapterError::Decode("load packet must have an empty body".into()));
                }
                world.load(WorldChunkPos::new(0, 0), empty_chunk());
                Ok(vec![Directive::Emit(ClientEvent::ChunkLoaded {
                    pos: ModelChunkPos::new(0, 0),
                })])
            }
            BLOCK_UPDATE_PACKET => {
                if payload.len() != 16 {
                    return Err(AdapterError::Decode(
                        "block update packet must carry three coordinates and one state id".into(),
                    ));
                }
                let x = i32::from_be_bytes(payload[0..4].try_into().unwrap());
                let y = i32::from_be_bytes(payload[4..8].try_into().unwrap());
                let z = i32::from_be_bytes(payload[8..12].try_into().unwrap());
                let state_id = u32::from_be_bytes(payload[12..16].try_into().unwrap());
                world.set_block(x, y, z, state_id);
                Ok(vec![Directive::Emit(ClientEvent::SectionBlocksChanged {
                    section: SectionPos::from(BlockPos::new(x, y, z)),
                    blocks: vec![
                        [
                            (x & 15) as u8,
                            (y & 15) as u8,
                            (z & 15) as u8,
                        ],
                    ],
                })])
            }
            ENTITY_SPAWN_PACKET => {
                if payload.len() != 16 {
                    return Err(AdapterError::Decode(
                        "entity spawn packet must carry an id and position".into(),
                    ));
                }
                let entity_id = i32::from_be_bytes(payload[0..4].try_into().unwrap());
                let x = f32::from_be_bytes(payload[4..8].try_into().unwrap());
                let y = f32::from_be_bytes(payload[8..12].try_into().unwrap());
                let z = f32::from_be_bytes(payload[12..16].try_into().unwrap());
                Ok(vec![Directive::Emit(ClientEvent::EntitySpawned {
                    entity_id,
                    uuid: Some(Uuid::from_u128(entity_id as u128)),
                    entity_type: Identifier::new("minecraft", "pig").unwrap(),
                    pos: Vec3::new(x.into(), y.into(), z.into()),
                    rotation: Rotation::default(),
                    velocity: None,
                })])
            }
            ENTITY_MOVE_PACKET => {
                if payload.len() != 16 {
                    return Err(AdapterError::Decode(
                        "entity move packet must carry an id and relative position".into(),
                    ));
                }
                let entity_id = i32::from_be_bytes(payload[0..4].try_into().unwrap());
                let x = f32::from_be_bytes(payload[4..8].try_into().unwrap());
                let y = f32::from_be_bytes(payload[8..12].try_into().unwrap());
                let z = f32::from_be_bytes(payload[12..16].try_into().unwrap());
                Ok(vec![Directive::Emit(ClientEvent::EntityMoved {
                    entity_id,
                    movement: EntityMovement::Relative(Vec3::new(x.into(), y.into(), z.into())),
                    rotation: None,
                    on_ground: true,
                })])
            }
            ENTITY_REMOVE_PACKET => {
                if payload.len() != 4 {
                    return Err(AdapterError::Decode(
                        "entity remove packet must carry one id".into(),
                    ));
                }
                let entity_id = i32::from_be_bytes(payload.try_into().unwrap());
                Ok(vec![Directive::Emit(ClientEvent::EntityRemoved {
                    entity_ids: vec![entity_id],
                })])
            }
            INVENTORY_CONTENT_PACKET => {
                if payload.len() != 4 {
                    return Err(AdapterError::Decode(
                        "inventory content packet must carry a stack count".into(),
                    ));
                }
                let count = i32::from_be_bytes(payload.try_into().unwrap());
                if !(1..=64).contains(&count) {
                    return Err(AdapterError::Decode(
                        "inventory content packet count must be between one and 64".into(),
                    ));
                }
                let mut items = vec![None; 46];
                items[INVENTORY_TARGET_SLOT] = Some(ItemStack {
                    item: Identifier::new("minecraft", "diamond").unwrap(),
                    count: count as u32,
                    components: ItemComponents::default(),
                });
                Ok(vec![Directive::Emit(ClientEvent::ContainerContent {
                    window_id: 0,
                    state_id: ContainerStateId::new(7),
                    items,
                    carried_item: None,
                })])
            }
            INVENTORY_SLOT_PACKET => {
                if !payload.is_empty() {
                    return Err(AdapterError::Decode(
                        "inventory slot packet must have an empty body".into(),
                    ));
                }
                Ok(vec![Directive::Emit(ClientEvent::InventorySlotChanged {
                    slot: 0,
                    item: None,
                })])
            }
            CONTAINER_OPEN_PACKET => {
                if !payload.is_empty() {
                    return Err(AdapterError::Decode(
                        "container open packet must have an empty body".into(),
                    ));
                }
                Ok(vec![Directive::Emit(ClientEvent::ScreenOpened {
                    window_id: CONTAINER_WINDOW_ID,
                    menu_type: Identifier::new("minecraft", "generic_9x3").unwrap(),
                    title: Text::literal("Generated container"),
                })])
            }
            CONTAINER_CONTENT_PACKET => {
                if payload.len() != 4 {
                    return Err(AdapterError::Decode(
                        "container content packet must carry a stack count".into(),
                    ));
                }
                let count = i32::from_be_bytes(payload.try_into().unwrap());
                if !(1..=64).contains(&count) {
                    return Err(AdapterError::Decode(
                        "container content packet count must be between one and 64".into(),
                    ));
                }
                let mut items = vec![None; 27 + 36];
                items[CONTAINER_TARGET_SLOT] = Some(ItemStack {
                    item: Identifier::new("minecraft", "diamond").unwrap(),
                    count: count as u32,
                    components: ItemComponents::default(),
                });
                Ok(vec![Directive::Emit(ClientEvent::ContainerContent {
                    window_id: CONTAINER_WINDOW_ID,
                    state_id: ContainerStateId::new(11),
                    items,
                    carried_item: None,
                })])
            }
            CONTAINER_SLOT_PACKET => {
                if payload.len() != 8 {
                    return Err(AdapterError::Decode(
                        "container slot packet must carry a slot and stack count".into(),
                    ));
                }
                let slot = i32::from_be_bytes(payload[0..4].try_into().unwrap());
                let count = i32::from_be_bytes(payload[4..8].try_into().unwrap());
                if slot != CONTAINER_TARGET_SLOT as i32 || !(0..=64).contains(&count) {
                    return Err(AdapterError::Decode(
                        "container slot packet has an invalid fixture slot or count".into(),
                    ));
                }
                let item = (count > 0).then(|| ItemStack {
                    item: Identifier::new("minecraft", "diamond").unwrap(),
                    count: count as u32,
                    components: ItemComponents::default(),
                });
                Ok(vec![Directive::Emit(ClientEvent::ContainerSlot {
                    window_id: CONTAINER_WINDOW_ID,
                    state_id: ContainerStateId::new(12),
                    slot,
                    item,
                })])
            }
            _ => Err(AdapterError::Unsupported(format!(
                "fixture packet id {packet_id}"
            ))),
        }
    }

    fn encode_action(
        &self,
        _state: ConnectionState,
        _action: &lodestone_model::ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        Ok(None)
    }
}

fn empty_chunk() -> LoadedChunk {
    let column = ChunkColumn::new(
        -64,
        24,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        AIR_ID,
        0,
    );
    LoadedChunk::new(column, ColumnLight::new(24), Heightmaps::new(), Vec::new())
}

fn profile() -> LoginProfile {
    LoginProfile {
        username: "DifferentialFixture".into(),
        uuid: Uuid::nil(),
    }
}

fn server() -> ServerAddress {
    ServerAddress {
        host: "differential-fixture".into(),
        port: 0,
    }
}

struct ClientStateOracle {
    handle: lodestone_client::ClientHandle,
    events: EventStream,
    peer: Connection<DuplexStream>,
    tick: u64,
    fault_after_ticks: Option<u64>,
    entity_fault_after_ticks: Option<u64>,
    inventory_fault_after_ticks: Option<u64>,
    container_fault_after_ticks: Option<u64>,
    runtime: Runtime,
}

impl ClientStateOracle {
    fn new(fault_after_ticks: Option<u64>) -> Self {
        Self::new_with_faults(fault_after_ticks, None, None, None)
    }

    fn new_entity(entity_fault_after_ticks: Option<u64>) -> Self {
        Self::new_with_faults(None, entity_fault_after_ticks, None, None)
    }

    fn new_inventory(inventory_fault_after_ticks: Option<u64>) -> Self {
        Self::new_with_faults(None, None, inventory_fault_after_ticks, None)
    }

    fn new_container(container_fault_after_ticks: Option<u64>) -> Self {
        let mut oracle = Self::new_with_faults(None, None, None, container_fault_after_ticks);
        oracle
            .send_packet(CONTAINER_OPEN_PACKET, &[])
            .expect("open fixture container");
        oracle
    }

    fn new_with_faults(
        fault_after_ticks: Option<u64>,
        entity_fault_after_ticks: Option<u64>,
        inventory_fault_after_ticks: Option<u64>,
        container_fault_after_ticks: Option<u64>,
    ) -> Self {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("client-state fixture runtime");
        let (client_io, server_io) = memory_pair();
        let (handle, events) = runtime.block_on(async {
            ClientBuilder::new(server(), profile(), Box::new(ScriptedAdapter)).connect_with(client_io)
        });
        let mut oracle = Self {
            handle,
            events,
            peer: Connection::new(server_io),
            tick: 0,
            fault_after_ticks,
            entity_fault_after_ticks,
            inventory_fault_after_ticks,
            container_fault_after_ticks,
            runtime,
        };
        oracle.send_packet(LOAD_PACKET, &[]).expect("load fixture chunk");
        oracle
    }

    fn send_packet(&mut self, packet_id: i32, payload: &[u8]) -> Result<(), String> {
        self.runtime.block_on(async {
            self.peer
                .write_packet(packet_id, payload)
                .await
                .map_err(|error| format!("write fixture packet: {error}"))?;
            self.events
                .recv()
                .await
                .ok_or_else(|| {
                    String::from("client driver ended before acknowledging fixture packet")
                })?;
            Ok(())
        })
    }

    fn block_state_id(&self, pos: (i32, i32, i32)) -> Option<u32> {
        self.handle.block_at(BlockPos::new(pos.0, pos.1, pos.2))
    }

    fn send_entity_action(&mut self, action: &EntityAction) -> Result<(), String> {
        let (packet_id, payload) = match action {
            EntityAction::Spawn { entity_id, pos } => {
                let mut payload = Vec::with_capacity(16);
                payload.extend_from_slice(&entity_id.to_be_bytes());
                payload.extend_from_slice(&(pos.0 as f32).to_be_bytes());
                payload.extend_from_slice(&(pos.1 as f32).to_be_bytes());
                payload.extend_from_slice(&(pos.2 as f32).to_be_bytes());
                (ENTITY_SPAWN_PACKET, payload)
            }
            EntityAction::Move { entity_id, delta } => {
                let mut payload = Vec::with_capacity(16);
                payload.extend_from_slice(&entity_id.to_be_bytes());
                payload.extend_from_slice(&(delta.0 as f32).to_be_bytes());
                payload.extend_from_slice(&(delta.1 as f32).to_be_bytes());
                payload.extend_from_slice(&(delta.2 as f32).to_be_bytes());
                (ENTITY_MOVE_PACKET, payload)
            }
            EntityAction::Remove { entity_id } => {
                (ENTITY_REMOVE_PACKET, entity_id.to_be_bytes().to_vec())
            }
        };
        self.send_packet(packet_id, &payload)
    }

    fn entity_snapshot(&self) -> HashMap<i32, Vec3> {
        let mut entities: HashMap<_, _> = self
            .handle
            .entities()
            .into_iter()
            .map(|entity| (entity.entity_id, entity.position))
            .collect();
        if self
            .entity_fault_after_ticks
            .is_some_and(|fault_after| self.tick >= fault_after)
            && let Some(position) = entities.get_mut(&10)
        {
            position.x += 10.0;
        }
        entities
    }

    fn entity_position(&self, entity_id: i32) -> Option<Vec3> {
        self.handle.entity(entity_id).map(|entity| entity.position)
    }

    fn send_inventory_action(&mut self, action: InventoryAction) -> Result<(), String> {
        let (packet_id, payload) = match action {
            InventoryAction::Content { count } => {
                (INVENTORY_CONTENT_PACKET, count.to_be_bytes().to_vec())
            }
            InventoryAction::ClearHotbar => (INVENTORY_SLOT_PACKET, Vec::new()),
        };
        self.send_packet(packet_id, &payload)
    }

    fn inventory_snapshot(&self) -> HashMap<usize, (String, i32)> {
        let menu = self.handle.player_menu();
        let mut items = (0..menu.slot_count())
            .filter_map(|slot| {
                menu.slot_item(slot)
                    .map(|stack| (slot, (stack.item().to_string(), stack.count())))
            })
            .collect::<HashMap<_, _>>();
        if self
            .inventory_fault_after_ticks
            .is_some_and(|fault_after| self.tick >= fault_after)
        {
            items.remove(&INVENTORY_TARGET_SLOT);
        }
        items
    }

    fn send_container_action(&mut self, action: ContainerAction) -> Result<(), String> {
        let (packet_id, payload) = match action {
            ContainerAction::Content { count } => {
                (CONTAINER_CONTENT_PACKET, count.to_be_bytes().to_vec())
            }
            ContainerAction::Slot { count } => {
                let mut payload = Vec::with_capacity(8);
                payload.extend_from_slice(&(CONTAINER_TARGET_SLOT as i32).to_be_bytes());
                payload.extend_from_slice(&count.to_be_bytes());
                (CONTAINER_SLOT_PACKET, payload)
            }
        };
        self.send_packet(packet_id, &payload)
    }

    fn container_snapshot(&self) -> HashMap<usize, (String, i32)> {
        let menu = self
            .handle
            .open_menu()
            .expect("the generated container must remain open")
            .menu;
        let mut items = (0..menu.slot_count())
            .filter_map(|slot| {
                menu.slot_item(slot)
                    .map(|stack| (slot, (stack.item().to_string(), stack.count())))
            })
            .collect::<HashMap<_, _>>();
        if self
            .container_fault_after_ticks
            .is_some_and(|fault_after| self.tick >= fault_after)
        {
            items.remove(&CONTAINER_TARGET_SLOT);
        }
        items
    }
}

impl Drop for ClientStateOracle {
    fn drop(&mut self) {
        self.handle.shutdown();
    }
}

impl WorldOracle for ClientStateOracle {
    type Error = String;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        let Action::SetBlock { pos, state } = action else {
            return Err("the client fixture accepts only SetBlock actions".into());
        };
        let state_id = match state.as_str() {
            AIR => AIR_ID,
            STONE => STONE_ID,
            other => return Err(format!("unknown fixture state {other}")),
        };
        let mut payload = Vec::with_capacity(16);
        payload.extend_from_slice(&pos.0.to_be_bytes());
        payload.extend_from_slice(&pos.1.to_be_bytes());
        payload.extend_from_slice(&pos.2.to_be_bytes());
        payload.extend_from_slice(&state_id.to_be_bytes());
        self.send_packet(BLOCK_UPDATE_PACKET, &payload)
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        self.tick += 1;
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let actual = self.block_state_id(pos);
        let actual = if self
            .fault_after_ticks
            .is_some_and(|fault_after| self.tick >= fault_after)
        {
            match actual {
                Some(AIR_ID) => Some(STONE_ID),
                Some(STONE_ID) => Some(AIR_ID),
                other => other,
            }
        } else {
            actual
        };
        Ok(candidates
            .iter()
            .find(|candidate| match candidate.as_str() {
                AIR => actual == Some(AIR_ID),
                STONE => actual == Some(STONE_ID),
                _ => false,
            })
            .cloned())
    }
}

#[derive(Clone, Default)]
struct IntegratedFixtureSource {
    edits: Arc<Mutex<HashMap<(i32, i32, i32), String>>>,
}

impl ChunkSource for IntegratedFixtureSource {
    fn column(&self, cx: i32, cz: i32) -> ServerChunkColumn {
        let mut column = ServerChunkColumn::new(-64, 384);
        let edits = self.edits.lock().expect("integrated source lock poisoned");
        for (&(x, y, z), state) in edits.iter() {
            if x.div_euclid(16) == cx && z.div_euclid(16) == cz {
                column.set_block(x.rem_euclid(16), y, z.rem_euclid(16), state);
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.edits
            .lock()
            .expect("integrated source lock poisoned")
            .get(&(x, y, z))
            .cloned()
            .unwrap_or_else(|| AIR.into())
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".into()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        self.edits
            .lock()
            .expect("integrated source lock poisoned")
            .insert((x, y, z), name.into());
    }
}

struct IntegratedFixtureProtocol;

impl ServerProtocol for IntegratedFixtureProtocol {
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

    fn encode_chunk(
        &self,
        _cx: i32,
        _cz: i32,
        _column: &ServerChunkColumn,
    ) -> ServerDirective {
        ServerDirective::None
    }

    fn end_chunk_batch(&self, _batch_size: i32) -> ServerDirective {
        ServerDirective::None
    }
}

struct IntegratedServerOracle {
    server: IntegratedServer,
    source: IntegratedFixtureSource,
    next_server_tick: u64,
    tick: u64,
    fault_after_ticks: Option<u64>,
    runtime: Runtime,
}

impl IntegratedServerOracle {
    fn new(fault_after_ticks: Option<u64>) -> Self {
        let runtime = Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("integrated-server fixture runtime");
        let source = IntegratedFixtureSource::default();
        let source_view = source.clone();
        let (server, _client_io) = {
            let _guard = runtime.enter();
            IntegratedServer::open_in_memory_with_mobs(
                IntegratedFixtureProtocol,
                source,
                (0..=0, 0..=0),
                (0, 0),
                0,
                0,
            )
        };
        let initial_tick = server
            .server_tick_count()
            .expect("integrated fixture must start its real tick loop");
        Self {
            server,
            source: source_view,
            next_server_tick: initial_tick + 1,
            tick: 0,
            fault_after_ticks,
            runtime,
        }
    }
}

impl WorldOracle for IntegratedServerOracle {
    type Error = String;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        let Action::SetBlock { pos, state } = action else {
            return Err("integrated fixture accepts only SetBlock actions".into());
        };
        if state != AIR && state != STONE {
            return Err(format!("unknown integrated fixture state {state}"));
        }
        self.source.set_block(pos.0, pos.1, pos.2, state);
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        let target = self.next_server_tick;
        let deadline = Instant::now() + Duration::from_secs(2);
        let server = &self.server;
        self.runtime.block_on(async move {
            loop {
                if server
                    .server_tick_count()
                    .is_some_and(|current| current >= target)
                {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "integrated server did not reach tick {target}"
                    ));
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })?;
        self.next_server_tick = target + 1;
        self.tick += 1;
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let actual = self.source.block_state(pos.0, pos.1, pos.2);
        let actual = if self
            .fault_after_ticks
            .is_some_and(|fault_after| self.tick >= fault_after)
        {
            match actual.as_str() {
                AIR => STONE,
                STONE => AIR,
                _ => actual.as_str(),
            }
        } else {
            actual.as_str()
        };
        Ok(candidates
            .iter()
            .find(|candidate| candidate.as_str() == actual)
            .cloned())
    }
}

#[derive(Default)]
struct ExpectedWorld {
    blocks: HashMap<(i32, i32, i32), String>,
}

impl WorldOracle for ExpectedWorld {
    type Error = Infallible;

    fn apply(&mut self, action: &Action) -> Result<(), Self::Error> {
        if let Action::SetBlock { pos, state } = action {
            self.blocks.insert(*pos, state.clone());
        }
        Ok(())
    }

    fn advance_tick(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn block_state(
        &mut self,
        pos: (i32, i32, i32),
        candidates: &[String],
    ) -> Result<Option<String>, Self::Error> {
        let actual = self.blocks.get(&pos).map_or(AIR, String::as_str);
        Ok(candidates.iter().find(|candidate| candidate.as_str() == actual).cloned())
    }
}

#[derive(Debug, Clone, Copy)]
enum EntityAction {
    Spawn { entity_id: i32, pos: (f64, f64, f64) },
    Move { entity_id: i32, delta: (f64, f64, f64) },
    Remove { entity_id: i32 },
}

#[derive(Debug, Clone, Copy)]
struct EntityScriptStep {
    tick: u64,
    action: EntityAction,
}

#[derive(Debug, Clone, PartialEq)]
struct EntityDivergence {
    tick: u64,
    entity_id: i32,
    left: Option<(f64, f64, f64)>,
    right: Option<(f64, f64, f64)>,
}

#[derive(Debug, Clone, PartialEq)]
enum EntityDifferentialOutcome {
    Agreed,
    Diverged(EntityDivergence),
}

#[derive(Default)]
struct EntityExpectedWorld {
    entities: HashMap<i32, Vec3>,
}

impl EntityExpectedWorld {
    fn apply(&mut self, action: EntityAction) {
        match action {
            EntityAction::Spawn { entity_id, pos } => {
                self.entities
                    .insert(entity_id, Vec3::new(pos.0, pos.1, pos.2));
            }
            EntityAction::Move { entity_id, delta } => {
                if let Some(position) = self.entities.get_mut(&entity_id) {
                    position.x += delta.0;
                    position.y += delta.1;
                    position.z += delta.2;
                }
            }
            EntityAction::Remove { entity_id } => {
                self.entities.remove(&entity_id);
            }
        }
    }

    fn snapshot(&self) -> HashMap<i32, Vec3> {
        self.entities.clone()
    }
}

fn fixed_entity_script() -> Vec<EntityScriptStep> {
    vec![
        EntityScriptStep {
            tick: 0,
            action: EntityAction::Spawn {
                entity_id: 10,
                pos: (0.0, 64.0, 0.0),
            },
        },
        EntityScriptStep {
            tick: 0,
            action: EntityAction::Spawn {
                entity_id: 11,
                pos: (5.0, 64.0, 5.0),
            },
        },
        EntityScriptStep {
            tick: 1,
            action: EntityAction::Move {
                entity_id: 10,
                delta: (1.0, 0.0, 0.0),
            },
        },
        EntityScriptStep {
            tick: 1,
            action: EntityAction::Remove { entity_id: 11 },
        },
    ]
}

const GENERATED_ENTITY_CASES: usize = 8;
const GENERATED_ENTITY_STEPS: usize = 8;

fn generated_entity_campaign() -> Vec<Vec<EntityScriptStep>> {
    (0..GENERATED_ENTITY_CASES)
        .map(|case| {
            let main_id = 10;
            let auxiliary_id = 100 + case as i32;
            let base = (case as f64, 64.0, -(case as f64));
            let auxiliary = (base.0 + 5.0, base.1, base.2 + 5.0);
            vec![
                EntityScriptStep {
                    tick: 0,
                    action: EntityAction::Spawn {
                        entity_id: main_id,
                        pos: base,
                    },
                },
                EntityScriptStep {
                    tick: 0,
                    action: EntityAction::Spawn {
                        entity_id: auxiliary_id,
                        pos: auxiliary,
                    },
                },
                EntityScriptStep {
                    tick: 1,
                    action: EntityAction::Move {
                        entity_id: main_id,
                        delta: ((case + 1) as f64, 0.0, 0.0),
                    },
                },
                EntityScriptStep {
                    tick: 1,
                    action: EntityAction::Move {
                        entity_id: auxiliary_id,
                        delta: (-1.0, 0.0, 1.0),
                    },
                },
                EntityScriptStep {
                    tick: 2,
                    action: EntityAction::Remove {
                        entity_id: auxiliary_id,
                    },
                },
                EntityScriptStep {
                    tick: 2,
                    action: EntityAction::Move {
                        entity_id: main_id,
                        delta: (1.0, 0.0, -(case as f64)),
                    },
                },
                EntityScriptStep {
                    tick: 3,
                    action: EntityAction::Spawn {
                        entity_id: auxiliary_id,
                        pos: (base.0 + 9.0, base.1, base.2 - 4.0),
                    },
                },
                EntityScriptStep {
                    tick: 4,
                    action: EntityAction::Remove {
                        entity_id: auxiliary_id,
                    },
                },
            ]
        })
        .collect()
}

fn run_entity_differential(
    script: &[EntityScriptStep],
    expected: &mut EntityExpectedWorld,
    client: &mut ClientStateOracle,
) -> EntityDifferentialOutcome {
    let last_tick = script.iter().map(|step| step.tick).max().unwrap_or(0);
    for tick in 0..=last_tick {
        for step in script.iter().filter(|step| step.tick == tick) {
            expected.apply(step.action);
            client
                .send_entity_action(&step.action)
                .expect("entity packet fixture");
        }
        client.advance_tick().expect("entity fixture tick");
        let left = expected.snapshot();
        let right = client.entity_snapshot();
        let mut ids: Vec<_> = left.keys().chain(right.keys()).copied().collect();
        ids.sort_unstable();
        ids.dedup();
        for entity_id in ids {
            if entity_id == 10 && client.entity_fault_after_ticks.is_none() {
                assert_eq!(client.entity_position(entity_id), right.get(&entity_id).copied());
            }
            let left_position = left.get(&entity_id).map(|position| {
                (position.x, position.y, position.z)
            });
            let right_position = right.get(&entity_id).map(|position| {
                (position.x, position.y, position.z)
            });
            if left_position != right_position {
                return EntityDifferentialOutcome::Diverged(EntityDivergence {
                    tick,
                    entity_id,
                    left: left_position,
                    right: right_position,
                });
            }
        }
    }
    EntityDifferentialOutcome::Agreed
}

#[derive(Debug, Clone, Copy)]
enum InventoryAction {
    Content { count: i32 },
    ClearHotbar,
}

#[derive(Debug, Clone, Copy)]
struct InventoryScriptStep {
    tick: u64,
    action: InventoryAction,
}

#[derive(Debug, Clone, PartialEq)]
struct InventoryDivergence {
    tick: u64,
    slot: usize,
    left: Option<(String, i32)>,
    right: Option<(String, i32)>,
}

#[derive(Debug, Clone, PartialEq)]
enum InventoryDifferentialOutcome {
    Agreed,
    Diverged(InventoryDivergence),
}

#[derive(Default)]
struct InventoryExpectedWorld {
    slots: HashMap<usize, (String, i32)>,
}

impl InventoryExpectedWorld {
    fn apply(&mut self, action: InventoryAction) {
        match action {
            InventoryAction::Content { count } => {
                self.slots.insert(
                    INVENTORY_TARGET_SLOT,
                    ("minecraft:diamond".into(), count),
                );
            }
            InventoryAction::ClearHotbar => {
                self.slots.remove(&INVENTORY_TARGET_SLOT);
            }
        }
    }

    fn snapshot(&self) -> HashMap<usize, (String, i32)> {
        self.slots.clone()
    }
}

fn fixed_inventory_script() -> Vec<InventoryScriptStep> {
    vec![
        InventoryScriptStep {
            tick: 0,
            action: InventoryAction::Content { count: 5 },
        },
        InventoryScriptStep {
            tick: 1,
            action: InventoryAction::ClearHotbar,
        },
    ]
}

const GENERATED_INVENTORY_CASES: usize = 8;
const GENERATED_INVENTORY_STEPS: usize = 8;

fn generated_inventory_campaign() -> Vec<Vec<InventoryScriptStep>> {
    (0..GENERATED_INVENTORY_CASES)
        .map(|case| {
            (0..GENERATED_INVENTORY_STEPS)
                .map(|step| {
                    let seed = case * 17 + step * 5;
                    let action = if step == 0 || seed % 3 != 0 {
                        InventoryAction::Content {
                            count: 1 + ((seed * 7) % 64) as i32,
                        }
                    } else {
                        InventoryAction::ClearHotbar
                    };
                    InventoryScriptStep {
                        tick: step as u64,
                        action,
                    }
                })
                .collect()
        })
        .collect()
}

fn run_inventory_differential(
    script: &[InventoryScriptStep],
    expected: &mut InventoryExpectedWorld,
    client: &mut ClientStateOracle,
) -> InventoryDifferentialOutcome {
    let last_tick = script.iter().map(|step| step.tick).max().unwrap_or(0);
    for tick in 0..=last_tick {
        for step in script.iter().filter(|step| step.tick == tick) {
            expected.apply(step.action);
            client
                .send_inventory_action(step.action)
                .expect("inventory packet fixture");
        }
        client.advance_tick().expect("inventory fixture tick");
        let left = expected.snapshot();
        let right = client.inventory_snapshot();
        if left != right {
            let mut slots: Vec<_> = left.keys().chain(right.keys()).copied().collect();
            slots.sort_unstable();
            slots.dedup();
            let slot = slots
                .into_iter()
                .find(|slot| left.get(slot) != right.get(slot))
                .expect("different inventory maps must name a differing slot");
            return InventoryDifferentialOutcome::Diverged(InventoryDivergence {
                tick,
                slot,
                left: left.get(&slot).cloned(),
                right: right.get(&slot).cloned(),
            });
        }
    }
    InventoryDifferentialOutcome::Agreed
}

#[derive(Debug, Clone, Copy)]
enum ContainerAction {
    Content { count: i32 },
    Slot { count: i32 },
}

#[derive(Debug, Clone, Copy)]
struct ContainerScriptStep {
    tick: u64,
    action: ContainerAction,
}

#[derive(Debug, Clone, PartialEq)]
struct ContainerDivergence {
    tick: u64,
    slot: usize,
    left: Option<(String, i32)>,
    right: Option<(String, i32)>,
}

#[derive(Debug, Clone, PartialEq)]
enum ContainerDifferentialOutcome {
    Agreed,
    Diverged(ContainerDivergence),
}

#[derive(Default)]
struct ContainerExpectedWorld {
    slots: HashMap<usize, (String, i32)>,
}

impl ContainerExpectedWorld {
    fn apply(&mut self, action: ContainerAction) {
        match action {
            ContainerAction::Content { count } | ContainerAction::Slot { count } => {
                if count == 0 {
                    self.slots.remove(&CONTAINER_TARGET_SLOT);
                } else {
                    self.slots.insert(
                        CONTAINER_TARGET_SLOT,
                        ("minecraft:diamond".into(), count),
                    );
                }
            }
        }
    }

    fn snapshot(&self) -> HashMap<usize, (String, i32)> {
        self.slots.clone()
    }
}

const GENERATED_CONTAINER_CASES: usize = 8;
const GENERATED_CONTAINER_STEPS: usize = 6;

fn generated_container_campaign() -> Vec<Vec<ContainerScriptStep>> {
    (0..GENERATED_CONTAINER_CASES)
        .map(|case| {
            (0..GENERATED_CONTAINER_STEPS)
                .map(|step| {
                    let seed = case * 19 + step * 7;
                    let action = if step == 0 {
                        ContainerAction::Content {
                            count: 2 + (seed % 12) as i32,
                        }
                    } else if case == 0 && step == 1 {
                        ContainerAction::Slot { count: 12 }
                    } else if seed % 4 == 0 {
                        ContainerAction::Slot { count: 0 }
                    } else {
                        ContainerAction::Slot {
                            count: 1 + (seed % 64) as i32,
                        }
                    };
                    ContainerScriptStep {
                        tick: step as u64,
                        action,
                    }
                })
                .collect()
        })
        .collect()
}

fn run_container_differential(
    script: &[ContainerScriptStep],
    expected: &mut ContainerExpectedWorld,
    client: &mut ClientStateOracle,
) -> ContainerDifferentialOutcome {
    let last_tick = script.iter().map(|step| step.tick).max().unwrap_or(0);
    for tick in 0..=last_tick {
        for step in script.iter().filter(|step| step.tick == tick) {
            expected.apply(step.action);
            client
                .send_container_action(step.action)
                .expect("container packet fixture");
        }
        client.advance_tick().expect("container fixture tick");
        let left = expected.snapshot();
        let right = client.container_snapshot();
        if left != right {
            let mut slots: Vec<_> = left.keys().chain(right.keys()).copied().collect();
            slots.sort_unstable();
            slots.dedup();
            let slot = slots
                .into_iter()
                .find(|slot| left.get(slot) != right.get(slot))
                .expect("different container maps must name a differing slot");
            return ContainerDifferentialOutcome::Diverged(ContainerDivergence {
                tick,
                slot,
                left: left.get(&slot).cloned(),
                right: right.get(&slot).cloned(),
            });
        }
    }
    ContainerDifferentialOutcome::Agreed
}

const GENERATED_CASES: usize = 24;
const GENERATED_STEPS_PER_CASE: usize = 12;

fn generated_block_campaign() -> Vec<Script> {
    (0..GENERATED_CASES)
        .map(|case| {
            let steps = (0..GENERATED_STEPS_PER_CASE)
                .map(|step| {
                    let seed = case * 97 + step * 31;
                    let pos = (
                        ((seed * 5) % 16) as i32,
                        -63 + (seed % 8) as i32,
                        ((seed * 11) % 16) as i32,
                    );
                    let state = if (case == 0 && step == 0) || (seed + case) % 4 == 0 {
                        STONE
                    } else {
                        AIR
                    };
                    ScriptStep {
                        tick: step as u64,
                        action: Action::SetBlock {
                            pos,
                            state: state.into(),
                        },
                    }
                })
                .collect();
            Script::new(steps)
        })
        .collect()
}

fn generated_region(script: &Script) -> Vec<((i32, i32, i32), Vec<String>)> {
    let mut region = Vec::new();
    for step in &script.steps {
        let Action::SetBlock { pos, .. } = &step.action else {
            continue;
        };
        if !region.iter().any(|(candidate, _)| *candidate == *pos) {
            region.push((*pos, vec![AIR.into(), STONE.into()]));
        }
    }
    region
}

fn fixed_script() -> Script {
    Script::new(vec![
        ScriptStep {
            tick: 0,
            action: Action::SetBlock {
                pos: TARGET,
                state: STONE.into(),
            },
        },
        ScriptStep {
            tick: 1,
            action: Action::SetBlock {
                pos: TARGET,
                state: AIR.into(),
            },
        },
    ])
}

fn region() -> Vec<((i32, i32, i32), Vec<String>)> {
    vec![(TARGET, vec![AIR.into(), STONE.into()])]
}

#[test]
fn fixed_packet_script_matches_the_client_state_after_each_tick() {
    let result = thread::spawn(|| {
        let mut expected = ExpectedWorld::default();
        let mut client = ClientStateOracle::new(None);
        run_differential(&fixed_script(), &region(), &mut expected, &mut client, 1)
    })
    .join()
    .expect("client-state differential thread");
    assert!(matches!(result, DifferentialOutcome::Agreed));
}

#[test]
fn injected_first_tick_state_fault_reports_tick_zero() {
    let result = thread::spawn(|| {
        let mut expected = ExpectedWorld::default();
        let mut client = ClientStateOracle::new(Some(1));
        run_differential(&fixed_script(), &region(), &mut expected, &mut client, 1)
    })
    .join()
    .expect("client-state differential control thread");
    match result {
        DifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 0);
            assert_eq!(divergence.pos, TARGET);
            assert_eq!(divergence.left.as_deref(), Some(STONE));
            assert_eq!(divergence.right.as_deref(), Some(AIR));
        }
        other => panic!("first client-state fault was not reported: {other:?}"),
    }
}

#[test]
fn fixed_entity_script_matches_client_entities_after_each_tick() {
    let result = thread::spawn(|| {
        let mut expected = EntityExpectedWorld::default();
        let mut client = ClientStateOracle::new_entity(None);
        run_entity_differential(
            &fixed_entity_script(),
            &mut expected,
            &mut client,
        )
    })
    .join()
    .expect("entity-state differential thread");
    assert!(matches!(result, EntityDifferentialOutcome::Agreed));
}

#[test]
fn injected_entity_fault_reports_the_first_diverging_tick() {
    let result = thread::spawn(|| {
        let mut expected = EntityExpectedWorld::default();
        let mut client = ClientStateOracle::new_entity(Some(1));
        run_entity_differential(
            &fixed_entity_script(),
            &mut expected,
            &mut client,
        )
    })
    .join()
    .expect("entity-state differential control thread");
    match result {
        EntityDifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 0);
            assert_eq!(divergence.entity_id, 10);
            assert_eq!(divergence.left, Some((0.0, 64.0, 0.0)));
            assert_eq!(divergence.right, Some((10.0, 64.0, 0.0)));
        }
        other => panic!("first entity-state fault was not reported: {other:?}"),
    }
}

#[test]
fn fixed_inventory_script_matches_client_inventory_after_each_tick() {
    let result = thread::spawn(|| {
        let mut expected = InventoryExpectedWorld::default();
        let mut client = ClientStateOracle::new_inventory(None);
        run_inventory_differential(
            &fixed_inventory_script(),
            &mut expected,
            &mut client,
        )
    })
    .join()
    .expect("inventory-state differential thread");
    assert!(matches!(result, InventoryDifferentialOutcome::Agreed));
}

#[test]
fn injected_inventory_fault_reports_the_first_diverging_tick() {
    let result = thread::spawn(|| {
        let mut expected = InventoryExpectedWorld::default();
        let mut client = ClientStateOracle::new_inventory(Some(1));
        run_inventory_differential(
            &fixed_inventory_script(),
            &mut expected,
            &mut client,
        )
    })
    .join()
    .expect("inventory-state differential control thread");
    match result {
        InventoryDifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 0);
            assert_eq!(divergence.slot, INVENTORY_TARGET_SLOT);
            assert_eq!(divergence.left, Some(("minecraft:diamond".into(), 5)));
            assert_eq!(divergence.right, None);
        }
        other => panic!("first inventory-state fault was not reported: {other:?}"),
    }
}

#[test]
fn generated_block_packet_campaign_matches_after_each_tick() {
    let campaign = generated_block_campaign();
    assert_eq!(campaign.len(), GENERATED_CASES);
    assert!(campaign
        .iter()
        .all(|script| script.steps.len() == GENERATED_STEPS_PER_CASE));
    for script in campaign {
        let region = generated_region(&script);
        assert!(!region.is_empty(), "generated script must name a probe region");
        let result = thread::spawn(move || {
            let mut expected = ExpectedWorld::default();
            let mut client = ClientStateOracle::new(None);
            run_differential(
                &script,
                &region,
                &mut expected,
                &mut client,
                0,
            )
        })
        .join()
        .expect("generated client-state differential thread");
        assert!(matches!(result, DifferentialOutcome::Agreed));
    }
}

#[test]
fn generated_campaign_fault_reports_the_first_diverging_tick() {
    let script = generated_block_campaign()
        .into_iter()
        .next()
        .expect("generated campaign has a first script");
    let result = thread::spawn(move || {
        let mut expected = ExpectedWorld::default();
        let mut client = ClientStateOracle::new(Some(1));
        run_differential(
            &script,
            &generated_region(&script),
            &mut expected,
            &mut client,
            0,
        )
    })
    .join()
    .expect("generated client-state control thread");
    match result {
        DifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 0);
            assert_eq!(divergence.pos, (0, -63, 0));
            assert_eq!(divergence.left.as_deref(), Some(STONE));
            assert_eq!(divergence.right.as_deref(), Some(AIR));
        }
        other => panic!("generated campaign fault was not reported: {other:?}"),
    }
}

#[test]
fn generated_entity_packet_campaign_matches_after_each_tick() {
    let campaign = generated_entity_campaign();
    assert_eq!(campaign.len(), GENERATED_ENTITY_CASES);
    assert!(campaign
        .iter()
        .all(|script| script.len() == GENERATED_ENTITY_STEPS));
    for script in campaign {
        let result = thread::spawn(move || {
            let mut expected = EntityExpectedWorld::default();
            let mut client = ClientStateOracle::new_entity(None);
            run_entity_differential(&script, &mut expected, &mut client)
        })
        .join()
        .expect("generated entity-state differential thread");
        assert!(matches!(result, EntityDifferentialOutcome::Agreed));
    }
}

#[test]
fn generated_entity_campaign_fault_reports_the_first_diverging_tick() {
    let script = generated_entity_campaign()
        .into_iter()
        .next()
        .expect("generated entity campaign has a first script");
    let result = thread::spawn(move || {
        let mut expected = EntityExpectedWorld::default();
        let mut client = ClientStateOracle::new_entity(Some(1));
        run_entity_differential(&script, &mut expected, &mut client)
    })
    .join()
    .expect("generated entity-state control thread");
    match result {
        EntityDifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 0);
            assert_eq!(divergence.entity_id, 10);
            assert_eq!(divergence.left, Some((0.0, 64.0, 0.0)));
            assert_eq!(divergence.right, Some((10.0, 64.0, 0.0)));
        }
        other => panic!("generated entity campaign fault was not reported: {other:?}"),
    }
}

#[test]
fn generated_inventory_packet_campaign_matches_after_each_tick() {
    let campaign = generated_inventory_campaign();
    assert_eq!(campaign.len(), GENERATED_INVENTORY_CASES);
    assert!(campaign
        .iter()
        .all(|script| script.len() == GENERATED_INVENTORY_STEPS));
    for script in campaign {
        let result = thread::spawn(move || {
            let mut expected = InventoryExpectedWorld::default();
            let mut client = ClientStateOracle::new_inventory(None);
            run_inventory_differential(&script, &mut expected, &mut client)
        })
        .join()
        .expect("generated inventory-state differential thread");
        assert!(matches!(result, InventoryDifferentialOutcome::Agreed));
    }
}

#[test]
fn generated_inventory_campaign_fault_reports_the_first_diverging_tick() {
    let script = generated_inventory_campaign()
        .into_iter()
        .next()
        .expect("generated inventory campaign has a first script");
    let result = thread::spawn(move || {
        let mut expected = InventoryExpectedWorld::default();
        let mut client = ClientStateOracle::new_inventory(Some(1));
        run_inventory_differential(&script, &mut expected, &mut client)
    })
    .join()
    .expect("generated inventory-state control thread");
    match result {
        InventoryDifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 0);
            assert_eq!(divergence.slot, INVENTORY_TARGET_SLOT);
            assert_eq!(divergence.left, Some(("minecraft:diamond".into(), 1)));
            assert_eq!(divergence.right, None);
        }
        other => panic!("generated inventory campaign fault was not reported: {other:?}"),
    }
}

#[test]
fn integrated_server_world_oracle_matches_direct_chunk_source_after_each_tick() {
    let result = thread::spawn(|| {
        let mut expected = ExpectedWorld::default();
        let mut server = IntegratedServerOracle::new(None);
        run_differential(&fixed_script(), &region(), &mut expected, &mut server, 1)
    })
    .join()
    .expect("integrated-server differential thread");
    assert!(matches!(result, DifferentialOutcome::Agreed));
}

#[test]
fn integrated_server_world_fault_reports_the_first_diverging_tick() {
    let result = thread::spawn(|| {
        let mut expected = ExpectedWorld::default();
        let mut server = IntegratedServerOracle::new(Some(1));
        run_differential(&fixed_script(), &region(), &mut expected, &mut server, 1)
    })
    .join()
    .expect("integrated-server differential control thread");
    match result {
        DifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 0);
            assert_eq!(divergence.pos, TARGET);
            assert_eq!(divergence.left.as_deref(), Some(STONE));
            assert_eq!(divergence.right.as_deref(), Some(AIR));
        }
        other => panic!("first integrated-server fault was not reported: {other:?}"),
    }
}

#[test]
fn generated_container_packet_campaign_matches_after_each_tick() {
    let campaign = generated_container_campaign();
    assert_eq!(campaign.len(), GENERATED_CONTAINER_CASES);
    assert!(campaign
        .iter()
        .all(|script| script.len() == GENERATED_CONTAINER_STEPS));
    for script in campaign {
        let result = thread::spawn(move || {
            let mut expected = ContainerExpectedWorld::default();
            let mut client = ClientStateOracle::new_container(None);
            run_container_differential(&script, &mut expected, &mut client)
        })
        .join()
        .expect("generated container-state differential thread");
        assert!(matches!(result, ContainerDifferentialOutcome::Agreed));
    }
}

#[test]
fn generated_container_campaign_fault_reports_the_first_diverging_tick() {
    let script = generated_container_campaign()
        .into_iter()
        .next()
        .expect("generated container campaign has a first script");
    let result = thread::spawn(move || {
        let mut expected = ContainerExpectedWorld::default();
        let mut client = ClientStateOracle::new_container(Some(2));
        run_container_differential(&script, &mut expected, &mut client)
    })
    .join()
    .expect("generated container-state control thread");
    match result {
        ContainerDifferentialOutcome::Diverged(divergence) => {
            assert_eq!(divergence.tick, 1);
            assert_eq!(divergence.slot, CONTAINER_TARGET_SLOT);
            assert_eq!(divergence.left, Some(("minecraft:diamond".into(), 12)));
            assert_eq!(divergence.right, None);
        }
        other => panic!("generated container-state control was not reported: {other:?}"),
    }
}
