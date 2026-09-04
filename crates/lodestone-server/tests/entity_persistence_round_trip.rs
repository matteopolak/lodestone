//! Entity and player persistence end to end, through the real production entry
//! point:
//! `IntegratedServer::open_persistent_with_mobs` → populate → shutdown →
//! **reopen** → the cow and the dropped diamond are still there.
//!
//! # Why this file exists at all rather than a spot check
//!
//! A restart can silently delete every mob and dropped item with **no error
//! anywhere** — the world opens, the join succeeds, the chunks stream, and the
//! animals are simply gone. Playing the game once cannot distinguish that from
//! "the cow wandered off", so the gate asserts the persisted records directly.
//!
//! # What this gate evidences, and what it does not
//!
//! | claim | evidenced by |
//! |---|---|
//! | our reading of vanilla's own entity NBT is correct | **externally**, in `entity_nbt_vanilla_oracle.rs`, against 2093 entities a real 26.2 server wrote |
//! | the region *container* is correct | **externally**, by `lodestone-anvil`'s real-`.mca` tests |
//! | a mob and an item survive close/reopen through the production path | **here** |
//! | a player's inventory and position survive a disconnect | **here** |
//!
//! Row three is a round trip through our own codec and is not dressed up as more
//! than that; what stops it being vacuous is that the schema it round-trips is
//! pinned against vanilla's bytes in the sibling file. Same argument
//! `world_persistence_round_trip.rs` makes for terrain, and the same trade: a
//! cheap deterministic terrain source, because this gate is about the disk.
//!
//! # Why reseeding requires a poll rather than a sleep
//!
//! `MobHandle::reseed` **replaces the whole `MobSim`** once the mob-seeding task
//! has generated its terrain off-thread. Anything spawned into the sim before
//! that point is discarded.
//! So a test that spawned immediately after `open()` would pass or fail on
//! scheduler timing, and — worse — would fail in the direction that looks like a
//! persistence bug.
//!
//! `MobSim::next_id` is the deterministic signal: `MobSim::new` starts at `1`,
//! `reseed` sets `1000`. Polling that is exact, where a `sleep` would be a guess.

use std::path::Path;
use std::time::Duration;

use lodestone_core::State;
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use lodestone_model::Vec3;
use uuid::Uuid;

/// A `ServerProtocol` that lowers nothing: this gate is about the disk, and the
/// connection exists only so the real constructor is the one under test.
#[derive(Debug)]
struct TestProtocol;

impl ServerProtocol for TestProtocol {
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

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// Flat, cheap, deterministic terrain. See this file's header for why the real
/// generator is not used.
#[derive(Debug)]
struct FlatWorld;

impl ChunkSource for FlatWorld {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                for y in MIN_Y..63 {
                    column.set_block(x, y, z, "minecraft:stone");
                }
                column.set_block(x, 63, z, "minecraft:grass_block[snowy=false]");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state(lx, y, lz)
            .to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(lx, y, lz)
            .to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// Deliberately **not** at the origin and deliberately in a different chunk from
/// the item below, so the per-chunk grouping in `EntityStorage::save` is really
/// exercised rather than collapsing to one chunk.
const COW_POS: Vec3 = Vec3 {
    x: 20.5,
    y: 64.0,
    z: 5.5,
};
const ITEM_POS: Vec3 = Vec3 {
    x: 4.5,
    y: 64.0,
    z: 4.5,
};
/// A species whose defaults differ from `MobSim::spawn`'s `minecraft:zombie`
/// fallback, so "the type was actually persisted" is distinguishable from "a mob
/// came back with the placeholder type".
const COW: &str = "minecraft:cow";
const DROPPED: &str = "minecraft:diamond";
const DROPPED_COUNT: u8 = 7;
/// Health the cow could not have by default (`combat_defaults` gives a cow 10.0),
/// so a restored value is distinguishable from a freshly spawned one.
const COW_HEALTH: f32 = 4.0;

async fn open(dir: &Path) -> IntegratedServer {
    let (server, _client, _world) = IntegratedServer::open_persistent_with_mobs(
        TestProtocol,
        dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=1, 0..=1),
        (8, 8),
        0,
        1,
        // An hour: every save in this gate is an explicit one, so a timer firing
        // mid-assertion cannot be mistaken for the thing under test.
        Duration::from_secs(3600),
    )
    .expect("open persistent world");
    server
}

/// Waits until the mob-seeding task has replaced the sim — see this file's header.
///
/// Returns `false` on timeout rather than hanging, so a broken seed task reports
/// as a named failure instead of a test that never finishes.
async fn wait_for_reseed(server: &IntegratedServer) -> bool {
    let mobs = server.mobs().expect("a persistent world has a mob sim");
    for _ in 0..600 {
        if mobs.with(|sim| sim.next_id()) >= 1000 {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    false
}

/// Waits until the sim holds at least one mob and one dropped item.
async fn wait_for_population(server: &IntegratedServer) -> (usize, usize) {
    let mobs = server.mobs().expect("a persistent world has a mob sim");
    let mut last = (0, 0);
    for _ in 0..600 {
        last = mobs.with(|sim| (sim.iter().count(), sim.item_count()));
        if last.0 > 0 && last.1 > 0 {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    last
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-302-303-q7x-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}

/// A mob and a dropped item survive close and reopen.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mob_and_a_dropped_item_survive_close_and_reopen() {
    let dir = tempdir("entities");

    // --- session one -------------------------------------------------------
    let server = open(&dir).await;
    assert!(
        wait_for_reseed(&server).await,
        "the mob seed task never ran; anything spawned now would be discarded"
    );
    let (cow_uuid, item_uuid) = server
        .mobs()
        .expect("mob sim")
        .with(|sim| {
            let cow = sim.spawn_species(COW.parse().expect("valid key"), COW_POS);
            cow.set_health(COW_HEALTH);
            let cow_uuid = cow.uuid();
            let item_id = sim.spawn_item(
                DROPPED.parse().expect("valid key"),
                ITEM_POS,
                Vec3::new(0.0, 0.0, 0.0),
                lodestone_entity::item_entity::ItemLifecycle {
                    age: 40,
                    pickup_delay: 0,
                    count: DROPPED_COUNT,
                    max_stack_size: 64,
                },
            );
            // Read back before shutdown, so a failure here is a live-sim defect
            // and not a persistence one — the two would be indistinguishable.
            assert_eq!(sim.item_count(), 1, "the live sim lost the drop immediately");
            let item_uuid = sim
                .saved_entities()
                .into_iter()
                .find(|e| e.item.is_some())
                .expect("the item is in the save snapshot")
                .uuid;
            let _ = item_id;
            (cow_uuid, item_uuid)
        });

    // `shutdown` flushes entities last; see `IntegratedServer::shutdown`.
    server.shutdown().await;

    // The file must actually exist. Without this, a reopen that restored nothing
    // *and* saved nothing would fail below with a confusing message about the
    // load path when the real defect was the save.
    let entities_dir = dir
        .join("dimensions")
        .join("minecraft")
        .join("overworld")
        .join("entities");
    let files: Vec<_> = std::fs::read_dir(&entities_dir)
        .expect("entities/ exists")
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("mca"))
        .collect();
    assert!(
        !files.is_empty(),
        "nothing was written to {}; the entity save never ran",
        entities_dir.display()
    );

    // --- session two -------------------------------------------------------
    let server = open(&dir).await;
    let (mob_count, item_count) = wait_for_population(&server).await;
    assert_eq!(
        (mob_count, item_count),
        (1, 1),
        "expected exactly one mob and one dropped item after reopen, got \
         {mob_count} mobs and {item_count} items"
    );

    let restored = server
        .mobs()
        .expect("mob sim")
        .with(|sim| sim.saved_entities());
    let cow = restored
        .iter()
        .find(|e| e.item.is_none())
        .expect("a mob came back");
    assert_eq!(
        cow.id.to_string(),
        COW,
        "the species was not persisted — a restored mob carrying the placeholder \
         type is the `minecraft:acacia_boat` defect in a different costume"
    );
    assert_eq!(
        cow.uuid, cow_uuid,
        "the uuid must round-trip: `EntityStorage::save` clears stale records by \
         uuid identity, so a regenerated one duplicates the mob on every restart"
    );
    assert!(
        (cow.pos.x - COW_POS.x).abs() < 1.0 && (cow.pos.z - COW_POS.z).abs() < 1.0,
        "the cow came back at {:?}, not near {COW_POS:?}",
        cow.pos
    );
    // Predicted, not merely "changed": a freshly spawned cow has 10.0, so this
    // number lands on the restored hypothesis and not on the default one.
    assert!(
        (cow.health.expect("a living mob has health") - COW_HEALTH).abs() < 0.001,
        "expected the saved health {COW_HEALTH}, got {:?}; 10.0 would mean the mob \
         was respawned fresh rather than restored",
        cow.health
    );

    let item = restored
        .iter()
        .find(|e| e.item.is_some())
        .expect("the dropped item came back");
    let (id, count) = item.item.clone().expect("checked above");
    assert_eq!(id.to_string(), DROPPED, "the dropped stack changed item");
    assert_eq!(count, DROPPED_COUNT, "the dropped stack changed count");
    assert_eq!(item.uuid, item_uuid, "the item's uuid must round-trip too");
    assert_eq!(
        item.age,
        Some(40),
        "the item's age must survive, or every reloaded drop restarts its 5-minute \
         despawn clock and the world fills with immortal litter"
    );

    server.shutdown().await;
}

/// **The negative control** for the gate above: with nothing ever saved, the same
/// assertions must fail.
///
/// Without this, a `wait_for_population` that returned `(1, 1)` because the demo
/// seeder happened to place a mob would read as a pass. `demo_mob_count` returns
/// `0` unless an env var is set — this asserts that premise rather than trusting
/// it, which is the "what else already paints here" check.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_fresh_world_has_no_entities_so_the_gate_above_cannot_pass_vacuously() {
    let dir = tempdir("entities-control");
    let server = open(&dir).await;
    assert!(wait_for_reseed(&server).await, "seed task never ran");
    // Give the restore every chance to put something here.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mobs, items) = server
        .mobs()
        .expect("mob sim")
        .with(|sim| (sim.iter().count(), sim.item_count()));
    assert_eq!(
        (mobs, items),
        (0, 0),
        "a world with no saved entities must hold none; {mobs} mobs and {items} \
         items means something other than the restore is populating this sim, and \
         the sibling gate proves nothing"
    );
    server.shutdown().await;
}

/// A player's inventory, position, health and game mode survive a disconnect.
///
/// Driven through [`lodestone_server::player_data::PlayerDataStore`] against the
/// world directory the real constructor created, which is the same store
/// `crate::server`'s join and disconnect paths reach through
/// `ChunkSource::world_registries`. That routing is what the assertion on
/// `world_registries` below pins — without it this would test the store in
/// isolation and prove nothing about whether a connection can see it, which is
/// precisely the island shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn player_inventory_and_position_survive_a_disconnect() {
    use lodestone_model::{GameMode, ItemStack, Rotation};
    use lodestone_server::experience::PlayerExperience;
    use lodestone_server::player_data::{PlayerData, PlayerDataStore};

    let dir = tempdir("player");
    let (server, _client, world) = IntegratedServer::open_persistent_with_mobs(
        TestProtocol,
        &dir,
        FlatWorld,
        MIN_Y,
        HEIGHT,
        (0..=0, 0..=0),
        (8, 8),
        0,
        1,
        Duration::from_secs(3600),
    )
    .expect("open persistent world");

    // **The wiring assertion.** A store the connection path cannot reach is an
    // island, and this is the exact accessor `serve_connection_inner` and
    // `serve_play` consult.
    let registries = world
        .world_registries()
        .expect("a persistent source answers Some");
    let store = registries
        .player_data
        .clone()
        .expect("a persistent world exposes its player store to the connection path");

    // `unique_username`-derived uuid: offline mode derives the account uuid from
    // the username, so a fixed name would share one persisted file with every
    // other test in this repo — the documented cause of a total chunk blackout on
    // a dead player's death screen.
    let uuid = Uuid::new_v4();
    assert!(
        store.read(uuid).expect("read a never-saved player").is_none(),
        "a player who has never saved must read as None, not as an empty player"
    );

    let mut inventory = lodestone_server::PlayerInventory::new();
    inventory.set_native(
        0,
        Some(ItemStack::new(
            "minecraft:netherite_pickaxe".parse().expect("valid"),
            1,
        )),
    );
    inventory.set_native(
        13,
        Some(ItemStack::new(
            "minecraft:cooked_beef".parse().expect("valid"),
            23,
        )),
    );
    assert!(inventory.set_selected_hotbar_slot(4), "slot 4 is in range");

    let pos = Vec3::new(-412.31, 79.0, 88.5);
    // 1557 points is level 31 with the bar at 50/121 — see
    // `lodestone_server::experience`'s own regime table. A level and a total that
    // are both non-zero and *different* is the point: `XpLevel` and `XpTotal` are
    // adjacent `Int`s, so a transposition writes a legal file that reads back as
    // level 1557.
    let mut experience = PlayerExperience::default();
    experience.give_points(1_557);
    assert_eq!(experience.level(), 31, "the seeded XP is level 31");
    let saved = PlayerData::capture(
        pos,
        Rotation::new(136.5, -12.25),
        7.5,
        140,
        GameMode::Creative,
        &inventory,
        experience,
        Vec::new(),
        lodestone_server::dimension::Dimension::Overworld,
    );
    store.write(uuid, &saved).expect("write player data");
    server.shutdown().await;

    // --- a new session reads it through a freshly constructed store ---------
    let reopened = PlayerDataStore::new(&dir).expect("store");
    let read = reopened
        .read(uuid)
        .expect("read")
        .expect("the player file survived");

    assert_eq!(read.spawn_state().pos, pos, "position did not survive");
    assert!(read.spawn_state().alive, "7.5 health is not dead");
    assert!(
        (read.health - 7.5).abs() < 0.001,
        "health came back as {}, not 7.5 — 20.0 would mean a fresh player",
        read.health
    );
    assert_eq!(read.air_supply, 140, "air supply did not survive");
    assert_eq!(
        read.game_mode,
        Some(GameMode::Creative),
        "game mode did not survive"
    );
    assert_eq!(
        read.selected_slot, 4,
        "the selected hotbar slot did not survive"
    );
    assert_eq!(
        read.experience.level(),
        31,
        "the XP level did not survive — 0 means the `Xp*` fields are unmodelled again"
    );
    assert_eq!(
        read.experience.total(),
        1_557,
        "the lifetime total did not survive; 31 here would be a level/total transposition"
    );
    assert!(
        // `1e-5`, not `1e-6`: reaching level 31 costs 31 `f32` carry re-expressions and
        // lands on 0.41322213 against 50/121 = 0.41322314. The nearest wrong answer
        // (level 30's cost of 112, giving 0.446) is far outside this.
        (read.experience.progress() - 50.0 / 121.0).abs() < 1e-5,
        "the bar came back at {}, not 50/121 — level 31 costs 121 points",
        read.experience.progress()
    );

    let back = read.to_inventory();
    assert_eq!(
        back.native(0).map(|s| s.item.to_string()),
        Some("minecraft:netherite_pickaxe".to_owned()),
        "hotbar slot 0 did not survive"
    );
    let beef = back.native(13).expect("slot 13 survived");
    assert_eq!(beef.item.to_string(), "minecraft:cooked_beef");
    assert_eq!(beef.count, 23, "the stack count did not survive");
    // Empty slots must stay empty rather than filling with the last read stack —
    // the sparse `Inventory` list's own failure mode.
    assert!(back.native(1).is_none(), "slot 1 must still be empty");
    assert_eq!(
        back.selected_hotbar_slot(),
        4,
        "the rebuilt inventory lost the selected slot"
    );
}

/// A mob that walks into another chunk must not be **duplicated** by the next
/// load.
///
/// This is the property `EntityStorage::save`'s uuid-identity clearing exists for,
/// and it is worth its own gate because the naive save — rewrite only the chunks
/// that currently hold entities — passes every assertion in the round-trip gate
/// above while doubling the world's mob population on every single restart. A
/// player would see it as "my farm keeps filling up", not as a persistence bug.
#[test]
fn a_mob_that_changes_chunk_is_moved_not_duplicated() {
    use lodestone_model::Rotation;
    use lodestone_server::entity_storage::{EntityStorage, SavedEntity};

    let dir = tempdir("stale");
    let storage = EntityStorage::new(&dir).expect("storage");

    let uuid = Uuid::new_v4();
    let mut cow = SavedEntity {
        id: COW.parse().expect("valid"),
        uuid,
        pos: Vec3::new(4.5, 64.0, 4.5),
        motion: Vec3::new(0.0, 0.0, 0.0),
        rotation: Rotation::new(0.0, 0.0),
        health: Some(10.0),
        item: None,
        age: None,
        pickup_delay: None,
        extra: Vec::new(),
    };
    assert_eq!(cow.chunk(), (0, 0));
    storage.save(std::slice::from_ref(&cow)).expect("first save");

    // Two chunks over, and — deliberately — still inside the same region file, so
    // the clearing has to work *within* one rewrite rather than only across files.
    cow.pos = Vec3::new(36.5, 64.0, 68.5);
    assert_eq!(cow.chunk(), (2, 4));
    storage.save(std::slice::from_ref(&cow)).expect("second save");

    let old = storage.load_chunk(0, 0).expect("load old chunk");
    assert!(
        old.is_empty(),
        "the cow's old chunk still holds {} record(s); the next load would spawn it twice",
        old.len()
    );
    let new = storage.load_chunk(2, 4).expect("load new chunk");
    assert_eq!(new.len(), 1, "the cow must be in exactly one chunk");
    assert_eq!(new[0].uuid, uuid);

    // And the whole-area load — the one world open actually uses — sees exactly one.
    let all = storage.load_area(-2..=6, -2..=6).expect("load area");
    assert_eq!(
        all.len(),
        1,
        "a full-area load found {} cows; expected 1",
        all.len()
    );
}

/// An entity this session does **not** own is preserved untouched by a save.
///
/// The control for the gate above: uuid-identity clearing must clear *ours* and
/// nothing else. Without this, "no duplicates" could be achieved by a save that
/// simply wipes the file — which would delete the 2093 mobs of a real vanilla
/// world the first time our sim (holding none of them) saved.
#[test]
fn a_save_does_not_delete_entities_this_session_never_owned() {
    use lodestone_model::Rotation;
    use lodestone_server::entity_storage::{EntityStorage, SavedEntity};

    let dir = tempdir("foreign");
    let storage = EntityStorage::new(&dir).expect("storage");

    let stranger = SavedEntity {
        id: "minecraft:sheep".parse().expect("valid"),
        uuid: Uuid::new_v4(),
        pos: Vec3::new(4.5, 64.0, 4.5),
        motion: Vec3::new(0.0, 0.0, 0.0),
        rotation: Rotation::new(0.0, 0.0),
        health: Some(8.0),
        item: None,
        age: None,
        pickup_delay: None,
        // A field we do not model, as every real vanilla mob carries dozens of.
        extra: vec![("PersistenceRequired".to_owned(), lodestone_core::Nbt::Byte(1))],
    };
    storage.save(std::slice::from_ref(&stranger)).expect("seed");

    // A different session, holding a different entity somewhere else entirely.
    let ours = SavedEntity {
        uuid: Uuid::new_v4(),
        pos: Vec3::new(200.5, 64.0, 200.5),
        ..stranger.clone()
    };
    storage.save(std::slice::from_ref(&ours)).expect("save ours");

    let kept = storage.load_chunk(0, 0).expect("load");
    assert_eq!(
        kept.len(),
        1,
        "the stranger was deleted by a save that had nothing to do with it"
    );
    assert_eq!(kept[0].uuid, stranger.uuid);
    assert_eq!(
        kept[0].extra, stranger.extra,
        "the stranger's unmodelled fields must survive verbatim"
    );
}

/// A file this build cannot read is **refused**, not silently replaced
/// when the file contains an unsupported data version.
///
/// This is the control that makes the refusal real: the same file with our own
/// `DataVersion` must read fine, so the failure below is about the version and
/// not about the file being malformed.
#[test]
fn a_player_file_from_another_game_version_is_refused_rather_than_overwritten() {
    use lodestone_core::Nbt;
    use lodestone_server::player_data::PlayerData;

    let ours = PlayerData::default().to_nbt();
    PlayerData::from_nbt(&ours).expect("our own version reads back");

    let Nbt::Compound(mut fields) = ours else {
        unreachable!("to_nbt builds a compound")
    };
    for (name, value) in &mut fields {
        if name == "DataVersion" {
            // 1.20.4's real data version — an actual older world, not a made-up
            // number.
            *value = Nbt::Int(3700);
        }
    }
    assert!(
        matches!(
            PlayerData::from_nbt(&Nbt::Compound(fields)),
            Err(lodestone_anvil::Error::UnsupportedDataVersion { found: Some(3700), .. })
        ),
        "an older DataVersion must be refused; reading it with 26.2's schema and \
         then writing it back is how a save gets destroyed"
    );
}
