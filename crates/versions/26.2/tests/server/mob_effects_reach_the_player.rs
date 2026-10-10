//! Two mob-simulation outputs that only a connection can deliver, proved on a
//! real client: a bucking horse's empty passenger list, and the timed effect a
//! splash potion leaves on a connected player. Each has a control that must not
//! produce the event.

use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_data::block_states::StateId;
use lodestone_model::{ClientEvent, Vec3};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    ChunkColumn, ChunkSource, MobHandle, MobOwner, PerceivedPlayer, PlayerIdentity, PlayerPerception,
    serve_connection,
};
use lodestone_v26_2::{V770ServerProtocol, adapter};
use uuid::Uuid;

struct AirSource;

impl ChunkSource for AirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(-64, 384)
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.column(x.div_euclid(16), z.div_euclid(16)).block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16)).biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16)).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {}
}

/// The connection's own entity id on the wire.
const LOCAL_PLAYER: i32 = 1;

fn floor() -> lodestone_server::ChunkWorld {
    let mut world = lodestone_server::ChunkWorld::new(-64, 384);
    let grass = lodestone_data::block_states::state_id("minecraft:grass_block").expect("grass block state");
    let grass = StateId::new(grass).expect("valid state id");
    for x in -20..20 {
        for z in -20..20 {
            world.set_block_id(x, 0, z, grass);
        }
    }
    world
}

fn player(uuid: Uuid) -> PerceivedPlayer {
    PerceivedPlayer {
        identity: Some(PlayerIdentity { uuid, entity_id: LOCAL_PLAYER }),
        perception: PlayerPerception {
            position: Vec3::new(0.5, 1.0, 0.5),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        },
    }
}

/// Connects a real client to `mobs`, ticking the simulation from the test, and
/// returns the client's event stream after `watch` has run for a while.
async fn collect_events(
    mobs: &MobHandle,
    uuid: Uuid,
    ticks: usize,
    act: impl FnOnce(&MobHandle),
    stop_when: impl Fn(&ClientEvent) -> bool,
) -> Vec<ClientEvent> {
    let (client_io, server_io) = memory_pair();
    let server_mobs = mobs.clone();
    let server_task = tokio::spawn(async move {
        let mut conn = Connection::new(server_io);
        serve_connection(
            &mut conn,
            &V770ServerProtocol,
            &AirSource,
            &server_mobs,
            0,
            &lodestone_server::BlockEntityHandle::default(),
            &server_mobs,
        )
        .await
    });
    let (handle, mut events) = ClientBuilder::new(
        ServerAddress { host: "memory".into(), port: 0 },
        LoginProfile { username: "Rider".into(), uuid },
        Box::new(adapter()),
    )
    .connect_with(client_io);
    let mut handle = handle;
    handle.wait_for_spawn(Duration::from_secs(30)).await.expect("client never spawned");
    // The connection feeds the simulation the position the client reports.
    handle.set_position(Vec3::new(0.5, 1.0, 0.5)).expect("client still connected");
    tokio::time::sleep(Duration::from_millis(300)).await;
    act(mobs);

    let mut seen = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    'outer: for _ in 0..ticks {
        mobs.with(|sim| sim.tick());
        tokio::time::sleep(Duration::from_millis(5)).await;
        while let Ok(Some(event)) = tokio::time::timeout(Duration::from_millis(1), events.recv()).await {
            let done = stop_when(&event);
            seen.push(event);
            if done {
                break 'outer;
            }
        }
        if std::time::Instant::now() > deadline {
            break;
        }
    }
    handle.shutdown();
    let _ = handle.join().await;
    let _ = tokio::time::timeout(Duration::from_secs(10), server_task).await;
    seen
}

fn horse_ride(tame: bool) -> (MobHandle, i32, Uuid) {
    let uuid = Uuid::new_v4();
    let mobs = MobHandle::new(floor());
    let id = mobs.with(|sim| {
        sim.set_next_id(1000);
        sim.set_players(vec![player(uuid)]);
        let id = sim.spawn_species("minecraft:horse".parse().unwrap(), Vec3::new(0.5, 1.0, 0.5)).id();
        let horse = sim.get_mut(id).expect("horse");
        horse.set_temper(0);
        if tame {
            horse.tame(MobOwner::Player(uuid));
        }
        assert!(sim.mount_mob(id, LOCAL_PLAYER));
        id
    });
    (mobs, id, uuid)
}

/// An untamed horse throws its rider and the client is told the horse has no
/// passengers; a tame horse (control) sends nothing of the kind.
#[tokio::test]
async fn a_bucking_horse_sends_the_empty_passenger_list_to_its_rider() {
    let emptied = |horse: i32| move |e: &ClientEvent| {
        matches!(e, ClientEvent::EntityPassengersChanged { vehicle_id, passenger_ids } if *vehicle_id == horse && passenger_ids.is_empty())
    };
    let (mobs, horse, uuid) = horse_ride(false);
    let events = collect_events(&mobs, uuid, 4000, |_| {}, emptied(horse)).await;
    assert!(events.iter().any(emptied(horse)), "the thrown rider was never told; saw {} events", events.len());

    let (mobs, horse, uuid) = horse_ride(true);
    let events = collect_events(&mobs, uuid, 1500, |_| {}, emptied(horse)).await;
    assert!(!events.iter().any(emptied(horse)), "control: a tame horse must not eject its rider");
}

/// A slowness splash landing on the connected player reaches its client as a
/// mob-effect update; the same splash 30 blocks away (control) does not.
#[tokio::test]
async fn a_splash_potion_applies_its_effect_to_the_connected_player() {
    let slowed = |e: &ClientEvent| {
        matches!(e, ClientEvent::MobEffectApplied { effect, .. } if effect.path() == "slowness")
    };
    let throw = |x: f64| {
        move |mobs: &MobHandle| {
            mobs.with(|sim| {
                let potion = lodestone_data::potion::potion_id("minecraft:slowness")
                    .and_then(lodestone_data::potion::PotionId::from_registry_id);
                sim.spawn_potion_projectile_from(
                    "minecraft:splash_potion".parse().unwrap(),
                    lodestone_entity::projectile::Projectile::throwable(
                        Vec3::new(x, 3.0, 0.5),
                        Vec3::new(0.0, -0.5, 0.0),
                    ),
                    None,
                    potion,
                );
            });
        }
    };
    let (uuid, mobs) = (Uuid::new_v4(), MobHandle::new(floor()));
    mobs.with(|sim| {
        sim.set_next_id(1000);
    });
    let events = collect_events(&mobs, uuid, 600, throw(0.5), slowed).await;
    assert!(events.iter().any(slowed), "the slowness splash never reached the player's client");

    let (uuid, mobs) = (Uuid::new_v4(), MobHandle::new(floor()));
    mobs.with(|sim| {
        sim.set_next_id(1000);
    });
    let events = collect_events(&mobs, uuid, 300, throw(30.5), slowed).await;
    assert!(!events.iter().any(slowed), "control: a splash 30 blocks away must not slow the player");
}
