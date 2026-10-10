//! **A player who uses an empty map and holds the result sees terrain fill in on it**, from a real
//! client over a real wire to the real server loop.
//!
//! Every link is production: the client's `/give` command and `UseItem` action, the server's map
//! creation, sampling and `map_item_data` encoding, the client's decode, and the game crate's
//! `MapStore`. The expected bytes are arithmetic from the map rules over a hand-built world
//! (map colour 1 for grass, 11 for stone, 12 for water; shade 0 low, 1 normal, 2 high), so they
//! do not come from the sampler under test.
//!
//! Verified as a control: with the server's per-tick map duty removed, the store stays empty and
//! this test fails at its first pixel assertion.

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_data::block_states::StateId;
use lodestone_game::maps::MapStore;
use lodestone_model::{BlockFace, BlockPos, ClientAction, ClientEvent, Hand, PredictionSequence, Rotation, Vec3f};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    ChunkColumn, ChunkSource, CommandDispatch, NoEntities, PlayerAwareSource, PlayerRegistry,
    serve_connection_with_commands,
};
use lodestone_v26_2::{V770ServerProtocol, adapter};

const BANNER: (i32, i32) = (10, 20);

fn state(text: &str) -> StateId {
    StateId::from_state_str(text).expect("fixture state")
}

/// Grass at y 64 over stone, with a one-block stone rise at block (5, -10) and a four-deep pool
/// at blocks (-63, -60) and (-63, -59).
fn block(x: i32, y: i32, z: i32) -> StateId {
    if (x, z) == (5, -10) && y == 65 {
        return state("minecraft:stone");
    }
    if (x, z) == BANNER && y == 65 {
        return state("minecraft:white_banner[rotation=0]");
    }
    if x == -63 && (-60..=-59).contains(&z) {
        return match y {
            ..=59 => state("minecraft:stone"),
            60 => state("minecraft:dirt"),
            61..=64 => state("minecraft:water[level=0]"),
            _ => StateId::AIR,
        };
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_held_new_map_fills_in_on_the_client_with_the_terrain_colours() {
    let (client_end, server_end) = memory_pair();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        let _ = serve_connection_with_commands(
            &mut conn,
            &V770ServerProtocol,
            &World,
            &PlayerAwareSource::new(NoEntities, PlayerRegistry::new()),
            0,
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &Default::default(),
            &CommandDispatch::none(),
        )
        .await;
    });

    let profile = LoginProfile { username: "Cartographer".into(), uuid: uuid::Uuid::from_u128(0x6a95) };
    let address = ServerAddress { host: "memory".into(), port: 0 };
    let (mut handle, mut events) =
        ClientBuilder::new(address, profile, Box::new(adapter())).connect_with(client_end);
    handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
    let spawn = handle.position().expect("spawned");
    assert_eq!((spawn.x.floor(), spawn.z.floor()), (0.0, 0.0), "premise: the map is made at the origin");

    handle.command("give @s minecraft:map").expect("send give");
    // Let the give settle into the selected slot before the use.
    tokio::time::sleep(Duration::from_millis(500)).await;
    handle
        .send_action(ClientAction::UseItem { hand: Hand::Main, rotation: Rotation::new(0.0, 0.0), sequence: 1 })
        .expect("send use");

    let mut store = MapStore::default();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let wanted = [
        (1usize, 1usize, (1u8 << 2) | 1),
        (69, 54, (11 << 2) | 2),
        (69, 55, 1 << 2),
        (1, 4, (12 << 2) | 1),
        (1, 5, (12 << 2) | 2),
    ];
    let mut done = false;
    while tokio::time::Instant::now() < deadline && !done {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(event @ ClientEvent::MapItemData { .. })) => {
                store.apply(&event);
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
        done = store.ids().next().is_some_and(|id| {
            let map = store.get(id).expect("listed id");
            wanted.iter().all(|&(x, y, color)| map.color_at(x, y) == color)
        });
    }

    let ids: Vec<_> = store.ids().collect();
    assert_eq!(ids.len(), 1, "exactly one map reaches the client");
    let map = store.get(ids[0]).unwrap();
    assert_eq!(map.scale, 0);
    for &(x, y, color) in &wanted {
        assert_eq!(map.color_at(x, y), color, "pixel ({x}, {y})");
    }
    // The holder's own marker is drawn.
    assert_eq!(map.decorations.len(), 1, "the holder's marker");

    // Using the map on the banner marks it: block (10, 20) is 10.5 and 20.5 blocks from the centre,
    // so the marker sits at 2 * d + 0.5 truncated, facing 180 degrees.
    handle
        .send_action(ClientAction::UseItemOn {
            hand: Hand::Main,
            pos: BlockPos::new(BANNER.0, 65, BANNER.1),
            face: BlockFace::Up,
            cursor: Vec3f::new(0.5, 0.0, 0.5),
            inside_block: false,
            sequence: PredictionSequence::new(2),
        })
        .expect("send banner click");
    let banner = |store: &MapStore| {
        let map = store.get(store.ids().next()?)?;
        map.decorations
            .iter()
            .find(|d| d.kind.to_string().ends_with("banner_white"))
            .map(|d| (d.x, d.y, d.rotation))
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while banner(&store).is_none() && tokio::time::Instant::now() < deadline {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(event @ ClientEvent::MapItemData { .. })) => {
                store.apply(&event);
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    assert_eq!(banner(&store), Some((21, 41, 8)), "the banner marker");

    handle.shutdown();
    server.abort();
}
