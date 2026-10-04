//! Protocol-776 travel with colliding entity IDs and publication revisions.
//! Expected entity-type IDs come from the generated 26.2 registry report.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};
use std::time::Duration;

use lodestone_core::{Ctx, Decode, Encode, Reader, Writer};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockPos, Vec3};
use lodestone_net::Connection;
use lodestone_server::{BlockEntityHandle, ChunkColumn, ChunkSource, EntitySource, NoEntities};
use lodestone_server::access::AccessHandle;
use lodestone_server::dimension::Dimension;
use lodestone_server::portal::PortalIndex;
use lodestone_server::world_state::WorldStateHandle;
use lodestone_server::TicketStoreHandle;
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packet_ids::{configuration, handshaking, login, play};
use lodestone_v26_2::packets::game::Respawn;
use lodestone_v26_2::packets::handshake::Intention;
use lodestone_v26_2::packets::login::LoginHello;
use tokio::io::DuplexStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 776 };
const DEADLINE: Duration = Duration::from_secs(15);

fn state(name: &str) -> StateId { StateId::from_state_str(name).expect("fixture state") }

#[derive(Clone)]
struct FixtureWorld {
    dimension: Dimension,
    edits: Arc<Mutex<HashMap<(Dimension, BlockPos), StateId>>>,
    portals: PortalIndex,
    fight_started: Arc<AtomicBool>,
    ticket_stores: Arc<[TicketStoreHandle; 3]>,
}

impl FixtureWorld {
    fn new() -> Self {
        let portals = PortalIndex::default();
        for dimension in [Dimension::Overworld, Dimension::Nether] {
            portals.insert(dimension, BlockPos::new(0, 64, 0));
        }
        Self {
            dimension: Dimension::Overworld,
            edits: Arc::new(Mutex::new(HashMap::new())),
            portals,
            fight_started: Arc::new(AtomicBool::new(false)),
            ticket_stores: Arc::new(std::array::from_fn(|_| TicketStoreHandle::new())),
        }
    }

    fn base_state(&self, x: i32, y: i32, z: i32) -> StateId {
        if y == 63 { return state("minecraft:stone"); }
        if self.dimension != Dimension::End && (x, y, z) == (0, 64, 0) {
            return state("minecraft:nether_portal[axis=x]");
        }
        if self.dimension == Dimension::Overworld && (x, y, z) == (4, 64, 0) {
            return state("minecraft:end_portal");
        }
        StateId::AIR
    }
}

impl ChunkSource for FixtureWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(self.dimension.min_y(), self.dimension.height());
        for z in 0..16 {
            for x in 0..16 {
                column.set_block_id(x, 63, z, state("minecraft:stone"));
                column.set_block_id(x, 64, z, self.base_state(cx * 16 + x, 64, cz * 16 + z));
            }
        }
        for (&(dimension, pos), &state) in self.edits.lock().unwrap().iter() {
            if dimension == self.dimension && pos.x.div_euclid(16) == cx && pos.z.div_euclid(16) == cz {
                column.set_block_id(pos.x.rem_euclid(16), pos.y, pos.z.rem_euclid(16), state);
            }
        }
        column
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> { Some(self.column(cx, cz)) }
    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.edits.lock().unwrap().get(&(self.dimension, BlockPos::new(x, y, z)))
            .copied().unwrap_or_else(|| self.base_state(x, y, z))
    }
    fn resident_block_state_id(&self, x: i32, y: i32, z: i32) -> Option<StateId> { Some(self.block_state_id(x, y, z)) }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String { "minecraft:plains".into() }
    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
        self.edits.lock().unwrap().insert((self.dimension, BlockPos::new(x, y, z)), state);
    }
    fn dimension(&self) -> Option<Dimension> { Some(self.dimension) }
    fn ticket_store(&self) -> Option<TicketStoreHandle> {
        Some(self.ticket_stores[match self.dimension {
            Dimension::Overworld => 0, Dimension::Nether => 1, Dimension::End => 2,
        }].clone())
    }
    fn reconcile_ticket_residency(&self) { self.ticket_store().unwrap().tick(); }
    fn sibling(&self, dimension: Dimension) -> Option<Arc<dyn ChunkSource>> {
        Some(Arc::new(Self { dimension, ..self.clone() }))
    }
    fn portal_index(&self) -> Option<&PortalIndex> { Some(&self.portals) }
    fn dragon_fight_started(&self) -> Option<bool> { Some(self.fight_started.load(Ordering::Acquire)) }
    fn claim_dragon_fight_start(&self) -> bool { !self.fight_started.swap(true, Ordering::AcqRel) }
}

async fn packet(client: &mut Connection<DuplexStream>) -> (i32, Vec<u8>) {
    tokio::time::timeout(DEADLINE, client.read_packet()).await
        .expect("bounded packet deadline").expect("packet read").expect("connection remains open")
}

async fn login(client: &mut Connection<DuplexStream>) {
    let mut writer = Writer::default();
    Intention { protocol_version: 776, host: "memory".into(), port: 25565, next_state: 2 }
        .encode(&mut writer, CTX).unwrap();
    client.write_packet(handshaking::serverbound::INTENTION, writer.as_slice()).await.unwrap();
    let mut writer = Writer::default();
    LoginHello { name: "DimensionViewer".into(), profile_id: Uuid::from_u128(91) }
        .encode(&mut writer, CTX).unwrap();
    client.write_packet(login::serverbound::HELLO, writer.as_slice()).await.unwrap();
    loop {
        let (id, payload) = packet(client).await;
        if id == login::clientbound::LOGIN_COMPRESSION {
            client.set_compression(Reader::new(&payload).var_i32().unwrap());
        } else {
            assert_eq!(id, login::clientbound::LOGIN_FINISHED);
            break;
        }
    }
    client.write_packet(login::serverbound::LOGIN_ACKNOWLEDGED, &[]).await.unwrap();
    while packet(client).await.0 != configuration::clientbound::FINISH_CONFIGURATION {}
    client.write_packet(configuration::serverbound::FINISH_CONFIGURATION, &[]).await.unwrap();
}

async fn acknowledge(client: &mut Connection<DuplexStream>, id: i32, payload: &[u8]) {
    if id == play::clientbound::PLAYER_POSITION {
        let mut ack = Writer::default();
        ack.var_i32(Reader::new(payload).var_i32().unwrap());
        client.write_packet(play::serverbound::ACCEPT_TELEPORTATION, ack.as_slice()).await.unwrap();
    } else if id == play::clientbound::CHUNK_BATCH_FINISHED {
        let mut ack = Writer::default();
        ack.f32(64.0);
        client.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, ack.as_slice()).await.unwrap();
        client.write_packet(play::serverbound::PLAYER_LOADED, &[]).await.unwrap();
    } else if id == play::clientbound::KEEP_ALIVE {
        client.write_packet(play::serverbound::KEEP_ALIVE, payload).await.unwrap();
    }
}

async fn movement(client: &mut Connection<DuplexStream>, x: f64, z: f64) {
    let mut writer = Writer::default();
    writer.f64(x);
    writer.f64(64.0);
    writer.f64(z);
    writer.u8(1);
    client.write_packet(play::serverbound::MOVE_PLAYER_POS, writer.as_slice()).await.unwrap();
}

async fn leg(client: &mut Connection<DuplexStream>, dimension: Dimension, mob_type: i32, peer: i32, changed: bool) -> Vec<(i32, i32)> {
    let mut respawn = !changed;
    let mut additions = Vec::new();
    let mut packet_counts = std::collections::BTreeMap::<i32, usize>::new();
    eprintln!("awaiting {dimension:?} leg: mob=(1000,{mob_type}), peer=({peer},156), changed={changed}");
    let result = tokio::time::timeout(DEADLINE, async {
        loop {
            let (id, payload) = packet(client).await;
            *packet_counts.entry(id).or_default() += 1;
            acknowledge(client, id, &payload).await;
            if id == play::clientbound::RESPAWN {
                assert_eq!(Respawn::decode(&mut Reader::new(&payload), CTX).unwrap().dimension, dimension.key());
                respawn = true;
                additions.clear();
            } else if id == play::clientbound::ADD_ENTITY {
                let mut reader = Reader::new(&payload);
                let entity_id = reader.var_i32().unwrap();
                reader.uuid().unwrap();
                additions.push((entity_id, reader.var_i32().unwrap()));
            }
            if respawn && additions.contains(&(1000, mob_type)) && additions.contains(&(peer, 156)) {
                return;
            }
        }
    }).await;
    assert!(result.is_ok(),
        "{dimension:?} leg timed out: respawn={respawn}, expected=[(1000,{mob_type}),({peer},156)], additions={additions:?}, packets={packet_counts:?}");
    additions
}

async fn leave_portal(client: &mut Connection<DuplexStream>, world: &WorldStateHandle, dimension: Dimension) {
    client.write_packet(play::serverbound::PLAYER_LOADED, &[]).await.unwrap();
    movement(client, 2.5, 0.5).await;
    tokio::time::timeout(DEADLINE, async {
        while !world.player_registry().perceptions(dimension).iter().any(|player| {
            player.identity.is_some_and(|identity| identity.uuid == Uuid::from_u128(91))
                && player.perception.position == Vec3::new(2.5, 64.0, 0.5)
        }) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }).await.expect("the short portal-exit movement must be accepted in the destination");
    let cooldown = Duration::from_millis(50)
        * (lodestone_server::portal::PLAYER_PORTAL_COOLDOWN as u32 + 2);
    let deadline = tokio::time::Instant::now() + cooldown;
    let mut corrections = 0;
    while let Ok(packet) = tokio::time::timeout_at(deadline, client.read_packet()).await {
        let (id, payload) = packet.unwrap().unwrap();
        if id == play::clientbound::PLAYER_POSITION { corrections += 1; }
        acknowledge(client, id, &payload).await;
    }
    assert_eq!(corrections, 0, "the short portal exit must not be corrected back into contact");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equal_coordinate_dimension_tickets_follow_arrival_respawn_and_disconnect() {
    let world = WorldStateHandle::new();
    let _ = world.ensure_dimension_runtime(Dimension::Overworld);
    let _ = world.ensure_dimension_runtime(Dimension::End);
    let fixture = FixtureWorld::new();
    fixture.fight_started.store(true, Ordering::Release);
    let home = fixture.ticket_stores[0].clone();
    let end = fixture.ticket_stores[2].clone();
    assert!(!home.same_store(&end), "fixture stores 0 and 2 have distinct identities");
    let server_fixture = fixture.clone();
    let server_world = world.clone();
    let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
    let server = tokio::spawn(async move {
        lodestone_server::serve_connection_with_access_and_state(
            &mut Connection::new(server_io), &V770ServerProtocol, &server_fixture, &NoEntities,
            0, &AccessHandle::default(), &server_world, &BlockEntityHandle::default(), None,
        ).await
    });
    let mut client = Connection::new(client_io);
    login(&mut client).await;
    loop {
        let (id, payload) = packet(&mut client).await;
        acknowledge(&mut client, id, &payload).await;
        if id == play::clientbound::CHUNK_BATCH_FINISHED { break; }
    }
    assert!(home.is_simulating((0, 0)));
    assert!(!end.is_resident((6, 0)), "the empty destination is a negative control");
    movement(&mut client, 100.5, 0.5).await;
    tokio::time::timeout(DEADLINE, async {
        while !home.is_simulating((6, 0)) { tokio::task::yield_now().await; }
    }).await.expect("movement grants the origin pair before the portal exists");
    assert!(home.is_resident((6, 0)));
    fixture.set_block(100, 64, 0, state("minecraft:end_portal"));
    movement(&mut client, 100.5, 0.5).await;
    await_dimension(&mut client, Dimension::End).await;
    await_ticket_anchor(&world, &end, Dimension::End, (6, 0)).await;
    assert!(!home.is_resident((6, 0)), "equal coordinates do not mean the same store");
    assert!(!home.is_simulating((6, 0)));
    client.write_packet(play::serverbound::PLAYER_LOADED, &[]).await.unwrap();
    assert!(world.player_registry().push_effect(Uuid::from_u128(91), lodestone_server::commands::Effect::Kill));
    tokio::time::timeout(DEADLINE, async {
        loop {
            let (id, payload) = packet(&mut client).await;
            acknowledge(&mut client, id, &payload).await;
            if id == play::clientbound::SET_HEALTH && Reader::new(&payload).f32().unwrap() == 0.0 { break; }
        }
    }).await.expect("queued kill must reach the health wire");
    client.write_packet(play::serverbound::CLIENT_COMMAND, &[0]).await.unwrap();
    await_dimension(&mut client, Dimension::Overworld).await;
    await_ticket_anchor(&world, &home, Dimension::Overworld, (0, 0)).await;
    assert!(!end.is_resident((6, 0)));
    assert!(!end.is_simulating((6, 0)));
    leave_portal(&mut client, &world, Dimension::Overworld).await;
    movement(&mut client, 100.5, 0.5).await;
    await_dimension(&mut client, Dimension::End).await;
    await_ticket_anchor(&world, &end, Dimension::End, (6, 0)).await;
    drop(client);
    tokio::time::timeout(DEADLINE, server).await.unwrap().unwrap().unwrap();
    home.tick();
    end.tick();
    assert!(!end.is_resident((6, 0)));
    assert!(!end.is_simulating((6, 0)));
    assert!(!home.is_simulating((0, 0)));
    assert!(home.is_resident((0, 0)), "the independent home spawn remains loaded");
    assert_eq!(world.player_registry().len(), 0);
}

async fn await_dimension(client: &mut Connection<DuplexStream>, dimension: Dimension) {
    tokio::time::timeout(DEADLINE, async {
        let mut respawned = false;
        loop {
            let (id, payload) = packet(client).await;
            acknowledge(client, id, &payload).await;
            if id == play::clientbound::RESPAWN {
                assert_eq!(Respawn::decode(&mut Reader::new(&payload), CTX).unwrap().dimension, dimension.key());
                respawned = true;
            } else if respawned && id == play::clientbound::PLAYER_POSITION {
                return;
            }
        }
    }).await.expect("dimension respawn must reach the wire");
}

async fn await_ticket_anchor(world: &WorldStateHandle, store: &TicketStoreHandle, dimension: Dimension, center: (i32, i32)) {
    tokio::time::timeout(DEADLINE, async {
        loop {
            let arrived = world.player_registry().perceptions(dimension).iter().any(|player| {
                let position = player.perception.position;
                player.identity.is_some_and(|identity| identity.uuid == Uuid::from_u128(91))
                    && ((position.x / 16.0).floor() as i32, (position.z / 16.0).floor() as i32) == center
            });
            if arrived && store.is_resident(center) && store.is_simulating(center) { return; }
            tokio::task::yield_now().await;
        }
    }).await.expect("wire arrival must publish the matching dimension ticket pair");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn equal_revision_travel_readds_destination_entities_and_routes_attacks() {
    let world = WorldStateHandle::new();
    world.set_rule("players_nether_portal_default_delay", "0").unwrap();
    let dimensions = [Dimension::Overworld, Dimension::Nether, Dimension::End];
    let species = ["minecraft:cow", "minecraft:zombified_piglin", "minecraft:enderman"];
    let runtimes = dimensions.map(|dimension| world.ensure_dimension_runtime(dimension));
    for (runtime, species) in runtimes.iter().zip(species) {
        let id = runtime.mobs().with(|sim| sim.spawn_species(species.parse().unwrap(), Vec3::new(2.5, 64.0, 0.5)).id());
        assert_eq!(id, 1000);
        runtime.publish_entities();
        assert_eq!(runtime.entities().snapshots_if_changed(None).unwrap().0, 1);
        assert!(runtime.entities().snapshots_if_changed(Some(1)).is_none());
    }
    let peers: Vec<_> = dimensions.into_iter().enumerate().map(|(index, dimension)| {
        world.player_registry().join_in_dimension("Peer", Uuid::from_u128(100 + index as u128), Vec3::new(8.0, 64.0, 8.0), dimension)
    }).collect();
    let primary_before = runtimes[0].mobs().snapshots();
    let (client_io, server_io) = tokio::io::duplex(4 * 1024 * 1024);
    let server_world = world.clone();
    let server = tokio::spawn(async move {
        lodestone_server::serve_connection_with_access_and_state(
            &mut Connection::new(server_io), &V770ServerProtocol, &FixtureWorld::new(), &NoEntities,
            0, &AccessHandle::default(), &server_world, &BlockEntityHandle::default(), None,
        ).await
    });
    let mut client = Connection::new(client_io);
    login(&mut client).await;
    let initial = leg(&mut client, Dimension::Overworld, 30, peers[0].entity_id(), false).await;
    assert!(!initial.iter().any(|&(id, _)| id == peers[1].entity_id() || id == peers[2].entity_id()));
    assert_eq!(world.player_registry().len(), 4);

    movement(&mut client, 0.5, 0.5).await;
    let nether = leg(&mut client, Dimension::Nether, 155, peers[1].entity_id(), true).await;
    assert!(!nether.iter().any(|&(id, _)| id == peers[0].entity_id() || id == peers[2].entity_id()));
    let before: Vec<_> = runtimes.iter().map(|runtime| runtime.mobs().with(|sim| sim.get(1000).unwrap().health())).collect();
    let mut attack = Writer::default();
    attack.var_i32(1000);
    client.write_packet(play::serverbound::ATTACK, attack.as_slice()).await.unwrap();
    tokio::time::timeout(DEADLINE, async {
        while runtimes[1].mobs().with(|sim| sim.get(1000).unwrap().health()) == before[1] {
            tokio::task::yield_now().await;
        }
    }).await.expect("attack must reach destination population");
    assert_eq!(runtimes[0].mobs().with(|sim| sim.get(1000).unwrap().health()), before[0]);
    assert_eq!(runtimes[2].mobs().with(|sim| sim.get(1000).unwrap().health()), before[2]);
    leave_portal(&mut client, &world, Dimension::Nether).await;
    movement(&mut client, 0.5, 0.5).await;
    leg(&mut client, Dimension::Overworld, 30, peers[0].entity_id(), true).await;
    leave_portal(&mut client, &world, Dimension::Overworld).await;
    movement(&mut client, 4.5, 0.5).await;
    let end = leg(&mut client, Dimension::End, 41, peers[2].entity_id(), true).await;
    assert!(!end.iter().any(|&(id, _)| id == peers[0].entity_id() || id == peers[1].entity_id()));
    assert_eq!(runtimes[0].mobs().snapshots(), primary_before, "End preparation cannot change the original population");
    assert!(runtimes[2].mobs().snapshots().iter().any(|entity| entity.entity_type.to_string() == "minecraft:ender_dragon"));
    runtimes[2].publish_entities();
    tokio::time::timeout(DEADLINE, async {
        loop {
            let (id, payload) = packet(&mut client).await;
            acknowledge(&mut client, id, &payload).await;
            if id == play::clientbound::ADD_ENTITY {
                let mut reader = Reader::new(&payload);
                reader.var_i32().unwrap();
                reader.uuid().unwrap();
                if reader.var_i32().unwrap() == 43 { break; }
            }
        }
    }).await.expect("End dragon must reach the destination wire");
    drop(client);
    // The client end is gone, so the connection ends either by reading the
    // close or by a periodic write hitting the closed pipe first; both are a
    // disconnect, and the withdrawal below is what is under test.
    let ended = tokio::time::timeout(DEADLINE, server).await.unwrap().unwrap();
    if let Err(error) = &ended {
        assert!(
            matches!(error, lodestone_server::ServerError::Net(_)),
            "only a transport error may end the connection: {error:?}"
        );
    }
    assert_eq!(world.player_registry().len(), 3, "disconnect withdraws only the live viewer");
}
