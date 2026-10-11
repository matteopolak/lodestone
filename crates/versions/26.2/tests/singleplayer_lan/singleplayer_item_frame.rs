//! **A player hangs an item frame, puts a filled map in it and turns it, and the client sees the
//! frame, the map and the frame marker**, from a real client over a real wire to the real server
//! loop.
//!
//! Every link is production: the client's `/give`, `UseItemOn`, `UseItem` and `InteractEntity`
//! actions, the server's frame placement, insertion, rotation and map-session duty, the 26.3
//! entity-data encoding, the client's decode, and the game crate's `MapStore`. The expected values
//! are arithmetic from the reference rules, not read from the code under test: the frame hangs in
//! the cell east of the clicked wall block, so its spawn position is that cell's corner and its yaw
//! is 270 (east is three quarter turns from south); a framed map shows a marker at the cell's
//! integer coordinates, `2 * d + 0.5` truncated half-pixels from the map centre (cell x 6, z 3 is
//! 12 and 6), turned `(270 + 8) * 16 / 360` truncated sixteenths (12).
//!
//! Verified as controls: with the use-on-block placement removed no frame spawns and the test fails
//! at the spawn wait; with the map session's frame list emptied the metadata still arrives but no
//! frame marker does, and it fails at the marker assertion.

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_data::block_states::StateId;
use lodestone_game::maps::MapStore;
use lodestone_model::{
    BlockFace, BlockPos, ClientAction, ClientEvent, EntityInteraction, Hand, ItemStack, PredictionSequence, Reported,
    Rotation, Vec3, Vec3f,
};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    ChunkColumn, ChunkSource, ChunkWorld, CommandDispatch, MobHandle, serve_connection_with_commands,
};
use lodestone_v26_2::{V770ServerProtocol, adapter};

/// The stone block the frame is hung on; the frame takes the air cell east of it.
const WALL: (i32, i32, i32) = (5, 65, 3);
const CELL: (i32, i32, i32) = (6, 65, 3);

fn state(text: &str) -> StateId {
    StateId::from_state_str(text).expect("fixture state")
}

/// Grass at y 64 over stone, with a two-high stone pillar at `WALL`.
fn block(x: i32, y: i32, z: i32) -> StateId {
    if (x, z) == (WALL.0, WALL.2) && (65..=66).contains(&y) {
        return state("minecraft:stone");
    }
    match y {
        ..=63 => state("minecraft:stone"),
        64 => state("minecraft:grass_block[snowy=false]"),
        _ => StateId::AIR,
    }
}

struct World;

impl ChunkSource for World {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(-64, 384);
        for lx in 0..16 {
            for lz in 0..16 {
                for y in -64..=66 {
                    let id = block(cx * 16 + lx, y, cz * 16 + lz);
                    if id != StateId::AIR {
                        column.set_block_id(lx, y, lz, id);
                    }
                }
            }
        }
        column
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        block(x, y, z)
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        Some(self.column(cx, cz))
    }
}

/// What the client has been told about the frame so far.
#[derive(Default, Debug)]
struct Seen {
    spawn: Option<(i32, Vec3, Rotation)>,
    item: Option<Option<ItemStack>>,
    rotation: Option<u8>,
}

struct Client {
    events: lodestone_client::EventStream,
    store: MapStore,
    seen: Seen,
}

impl Client {
    fn apply(&mut self, event: ClientEvent) {
        match &event {
            ClientEvent::EntitySpawned { entity_id, entity_type, pos, rotation, .. }
                if entity_type.path() == "item_frame" =>
            {
                self.seen.spawn = Some((*entity_id, *pos, *rotation));
            }
            ClientEvent::EntityMetadataUpdated { entity_id, metadata }
                if self.seen.spawn.is_some_and(|(id, ..)| id == *entity_id) =>
            {
                if let Reported::Reported(item) = &metadata.item {
                    self.seen.item = Some(item.clone());
                }
                if let Some(rotation) = metadata.item_frame_rotation {
                    self.seen.rotation = Some(rotation);
                }
            }
            ClientEvent::MapItemData { .. } => {
                self.store.apply(&event);
            }
            _ => {}
        }
    }

    /// Pumps events until `done` holds, or the deadline passes. Returns whether it held.
    async fn until(&mut self, seconds: u64, done: impl Fn(&Client) -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(seconds);
        while !done(self) {
            match tokio::time::timeout_at(deadline, self.events.recv()).await {
                Ok(Some(event)) => self.apply(event),
                Ok(None) | Err(_) => return done(self),
            }
        }
        true
    }

    fn markers(&self) -> Vec<(String, i8, i8, u8)> {
        self.store
            .ids()
            .flat_map(|id| self.store.get(id).expect("listed id").decorations.iter().cloned().collect::<Vec<_>>())
            .map(|d| (d.kind.path().to_owned(), d.x, d.y, d.rotation))
            .collect()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hung_frame_takes_a_filled_map_turns_it_and_marks_it_on_the_map() {
    let (client_end, server_end) = memory_pair();
    // The mob handle is both the entity source the connection streams and the sim a frame lives in.
    let mobs = MobHandle::new(ChunkWorld::new(-64, 384));
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = serve_connection_with_commands(
            &mut conn,
            &V770ServerProtocol,
            &World,
            &mobs,
            0,
            &Default::default(),
            &mobs,
            &Default::default(),
            &Default::default(),
            &CommandDispatch::none(),
        )
        .await;
    });

    let profile = LoginProfile { username: "Framer".into(), uuid: uuid::Uuid::from_u128(0xf4a3e) };
    let address = ServerAddress { host: "memory".into(), port: 0 };
    let (mut handle, events) = ClientBuilder::new(address, profile, Box::new(adapter())).connect_with(client_end);
    handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
    let spawn = handle.position().expect("spawned");
    assert_eq!((spawn.x.floor(), spawn.z.floor()), (0.0, 0.0), "premise: the map is made at the origin");
    let mut client = Client { events, store: MapStore::default(), seen: Seen::default() };

    // Survival, so each item is consumed from the selected slot and the next one lands there.
    handle.command("gamemode survival").expect("send gamemode");
    tokio::time::sleep(Duration::from_millis(300)).await;
    handle.command("give @s minecraft:item_frame").expect("send give");
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Hang the frame on the east face of the pillar.
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: BlockPos::new(WALL.0, WALL.1, WALL.2),
            face: BlockFace::East,
            cursor: Vec3f::new(1.0, 0.5, 0.5),
            inside_block: false,
            sequence: PredictionSequence::new(1),
        })
        .expect("send place");
    assert!(
        client.until(20, |c| c.seen.spawn.is_some() && c.seen.item.is_some() && c.seen.rotation.is_some()).await,
        "the client never saw a frame with its entity data: {:?}",
        client.seen
    );
    let (frame_id, pos, rotation) = client.seen.spawn.expect("spawned");
    assert_eq!(pos, Vec3::new(f64::from(CELL.0), f64::from(CELL.1), f64::from(CELL.2)), "the cell's corner");
    // The wire angle is a signed byte of 256ths of a turn, so 270 degrees arrives as -90.
    assert_eq!((rotation.yaw.rem_euclid(360.0), rotation.pitch), (270.0, 0.0), "facing east");
    assert_eq!(client.seen.item, Some(None), "an empty frame");
    assert_eq!(client.seen.rotation, Some(0));

    // An empty map becomes a filled one in the hand.
    handle.command("give @s minecraft:map").expect("send give");
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle
        .send_action(ClientAction::UseItem { hand: Hand::Main, rotation: Rotation::new(0.0, 0.0), sequence: 2 })
        .expect("send use");
    assert!(client.until(20, |c| c.store.ids().next().is_some()).await, "the filled map never reached the client");
    let map_id = client.store.ids().next().expect("one map").raw();
    assert!(
        client.until(10, |c| c.markers().iter().any(|m| m.0 == "player")).await,
        "premise: the holder is marked while the map is in hand"
    );

    // Put it in the frame.
    let interact = Hand::Main;
    handle
        .send_action(ClientAction::InteractEntity {
            entity_id: frame_id,
            interaction: EntityInteraction::Interact { hand: interact },
            sneaking: false,
        })
        .expect("send insert");
    assert!(
        client.until(20, |c| matches!(&c.seen.item, Some(Some(stack)) if stack.components.map_id == Some(map_id))).await,
        "the framed map never reached the client: {:?}",
        client.seen
    );
    let framed = client.seen.item.clone().flatten().expect("framed");
    assert_eq!((framed.item.to_string(), framed.count), ("minecraft:filled_map".to_owned(), 1));

    // The frame marker, and only it: the player no longer holds the map.
    let wanted = ("frame".to_owned(), 12, 6, 12);
    assert!(
        client.until(20, |c| c.markers() == [wanted.clone()]).await,
        "the map should carry exactly the frame marker {wanted:?}, has {:?}",
        client.markers()
    );

    // Using the frame again turns the item an eighth each time.
    for expected in [1u8, 2] {
        handle
            .send_action(ClientAction::InteractEntity {
                entity_id: frame_id,
                interaction: EntityInteraction::Interact { hand: interact },
                sneaking: false,
            })
            .expect("send rotate");
        assert!(
            client.until(20, |c| c.seen.rotation == Some(expected)).await,
            "rotation never became {expected}: {:?}",
            client.seen
        );
    }
    assert_eq!(client.seen.item.clone().flatten().map(|s| s.components.map_id), Some(Some(map_id)), "still framed");

    handle.shutdown();
    server.abort();
}
