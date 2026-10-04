//! Respawn, respawn-anchor and respawn-persistence tests that drive the
//! server's private entry points (`apply_client_command`, `apply_use_item_on`
//! and the End-exit respawn) directly with recording protocol doubles.

use super::*;
use super::DimensionOnly;

/// A protocol double carrying only what `apply_client_command`'s
/// `action == 0` arm calls — `encode_respawn`, `encode_set_health`,
/// `encode_air_supply_update` — everything else `unimplemented!()`, so a
/// call to a method this test does not expect is a panic, not a silent gap.
/// Records which respawn-related frames were requested, in order, as
/// short strings, because every directive here is `ServerDirective::None`.
#[derive(Default)]
struct RespawnOnlyProto(std::sync::Mutex<Vec<String>>);

impl RespawnOnlyProto {
    fn log(&self, entry: String) {
        self.0.lock().unwrap().push(entry);
    }
}

impl ServerProtocol for RespawnOnlyProto {
    fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
        unimplemented!("this test drives the function directly, never through decode")
    }
    fn login_success(&self, _u: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_play(&self, _r: i32) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        unimplemented!()
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, _c: &ChunkColumn) -> ServerDirective {
        unimplemented!()
    }
    fn end_chunk_batch(&self, _n: i32) -> ServerDirective {
        unimplemented!()
    }
    fn encode_respawn(&self, spawn: Vec3) -> Vec<ServerDirective> {
        self.log(format!("respawn {} {} {}", spawn.x, spawn.y, spawn.z));
        vec![ServerDirective::None]
    }
    fn encode_dimension_change(
        &self,
        dimension: &str,
        spawn: Vec3,
        _mode: GameMode,
    ) -> Vec<ServerDirective> {
        self.log(format!("dimension {dimension} {} {} {}", spawn.x, spawn.y, spawn.z));
        vec![ServerDirective::None]
    }
    fn encode_system_chat(&self, message: &str) -> ServerDirective {
        self.log(format!("chat {message}"));
        ServerDirective::None
    }
    fn supports_dimension_change(&self) -> bool {
        true
    }
    fn encode_game_event(&self, event: u8, value: f32) -> ServerDirective {
        self.log(format!("event {event} {value}"));
        ServerDirective::None
    }
    fn encode_block_update(&self, x: i32, y: i32, z: i32, state: StateId) -> ServerDirective {
        self.log(format!("block {x} {y} {z} charges={}", crate::respawn_anchor::charges(state)));
        ServerDirective::None
    }
    fn encode_container_slot(
        &self,
        _window_id: i32,
        _state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        self.log(format!("slot {slot} count={}", item.map_or(0, |stack| stack.count)));
        ServerDirective::None
    }
    fn encode_world_effect(&self, effect: &crate::effects::WorldEffect) -> ServerDirective {
        if let crate::effects::WorldEffect::Sound { sound, .. } = effect {
            self.log(format!("sound {sound}"));
        }
        ServerDirective::None
    }
    fn encode_set_health(&self, _health: f32, _food: i32, _saturation: f32) -> ServerDirective {
        ServerDirective::None
    }
    fn encode_air_supply_update(&self, _air: i32) -> ServerDirective {
        ServerDirective::None
    }
}

/// A block world for the respawn tests: air everywhere but the cells put in
/// it, labelled with a dimension, and able to hand out linked sibling worlds.
struct BlockWorld {
    dimension: crate::dimension::Dimension,
    blocks: std::sync::Mutex<std::collections::HashMap<(i32, i32, i32), StateId>>,
    siblings: std::sync::Mutex<Vec<Arc<BlockWorld>>>,
}

impl BlockWorld {
    /// A stone floor at y = 63 under 7 by 7 cells.
    fn flat(dimension: crate::dimension::Dimension) -> Self {
        let world = Self {
            dimension,
            blocks: std::sync::Mutex::new(std::collections::HashMap::new()),
            siblings: std::sync::Mutex::new(Vec::new()),
        };
        let stone = StateId::from_state_str("minecraft:stone").unwrap();
        for x in -3..=3 {
            for z in -3..=3 {
                world.set_block(x, 63, z, stone);
            }
        }
        world
    }

    /// A flat world with a respawn anchor at (0, 64, 0) holding `charges`.
    fn with_anchor(dimension: crate::dimension::Dimension, charges: u8) -> Self {
        let world = Self::flat(dimension);
        world.set_block(0, 64, 0, crate::respawn_anchor::with_charges(charges));
        world
    }

    /// A flat world with a bed whose foot is at (0, 64, 0).
    fn with_bed(dimension: crate::dimension::Dimension) -> Self {
        let world = Self::flat(dimension);
        world.set_block(
            0,
            64,
            0,
            StateId::from_state_str("minecraft:red_bed[facing=north,part=foot]").unwrap(),
        );
        world
    }

    /// Makes `other` reachable as this world's sibling for its dimension.
    fn link(&self, other: &Arc<BlockWorld>) {
        self.siblings.lock().unwrap().push(Arc::clone(other));
    }
}

impl ChunkSource for BlockWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 256);
        for (&(x, y, z), &state) in self.blocks.lock().unwrap().iter() {
            if x.div_euclid(16) == cx && z.div_euclid(16) == cz && (0..256).contains(&y) {
                column.set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), state);
            }
        }
        column
    }
    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        self.blocks.lock().unwrap().get(&(x, y, z)).copied().unwrap_or_else(crate::chunk::air_state)
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        crate::chunk::DEFAULT_BIOME.to_string()
    }
    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
        self.blocks.lock().unwrap().insert((x, y, z), state);
    }
    fn dimension(&self) -> Option<crate::dimension::Dimension> {
        Some(self.dimension)
    }
    fn sibling(&self, dimension: crate::dimension::Dimension) -> Option<Arc<dyn ChunkSource>> {
        self.siblings
            .lock()
            .unwrap()
            .iter()
            .find(|world| world.dimension == dimension)
            .map(|world| Arc::clone(world) as Arc<dyn ChunkSource>)
    }
}

/// What a driven respawn left behind.
struct RespawnRun {
    reset: Option<connection_travel::DimensionReset>,
    point: Option<RespawnPoint>,
    log: Vec<String>,
    feed: BlockTickFeed,
}

const RESPAWN_WORLD_SPAWN: Vec3 = Vec3 { x: 11.0, y: 71.0, z: -4.0 };

/// Which respawn a drive performs.
#[derive(Clone, Copy, PartialEq)]
enum Respawn {
    /// A death.
    Death,
    /// Leaving the End, which keeps the player's data.
    EndExit,
}

/// Drives a respawn through the real entry points: `apply_client_command`'s
/// `PERFORM_RESPAWN` arm for a death and `perform_respawn` for the End exit.
/// `home` is the dimension the connection joined in and `current` the one the
/// player is in.
async fn drive_respawn_between(
    kind: Respawn,
    home: &dyn ChunkSource,
    current: &dyn ChunkSource,
    point: Option<RespawnPoint>,
) -> RespawnRun {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let mut vitals = PlayerVitals::default();
    // The precondition every existing respawn test pins too: no health, no
    // air, matching `PlayerVitals::respawn`'s own before-state.
    vitals.kill();
    let mut burn = crate::burning::BurnState::new();
    burn.ignite_for_ticks(crate::burning::LAVA_IGNITE_TICKS);
    let mut fall = FallTracker::default();
    let mut teleport_acknowledgements = None;
    let world = crate::world_state::WorldStateHandle::default();
    let mut advancements =
        AdvancementManager::new(Vec::new()).expect("an empty advancement tree is valid");
    let mut client_loaded = true;
    let mut dimension_reset: Option<connection_travel::DimensionReset> = None;
    let mut point = point;
    let proto = RespawnOnlyProto::default();
    let feed = BlockTickFeed::default();

    match kind {
        Respawn::Death => {
            let mut end_exit = connection_travel::EndExit::new(false);
            apply_client_command(
                &mut conn,
                &proto,
                &mut state,
                &mut vitals,
                &mut burn,
                &mut fall,
                &mut teleport_acknowledgements,
                RESPAWN_WORLD_SPAWN,
                &mut point,
                current,
                home,
                &world,
                &mut advancements,
                Uuid::nil(),
                0, // PERFORM_RESPAWN
                4, // irrelevant here: only the `REQUEST_GAMERULE_VALUES` arm reads it
                &mut client_loaded,
                &mut dimension_reset,
                &mut end_exit,
                GameMode::Survival,
                &feed,
            )
            .await
            .expect("the fixture protocol never errors");
            assert_eq!(burn.remaining(), 0, "respawn must clear the old life's fire");
        }
        Respawn::EndExit => {
            dimension_reset = connection_travel::perform_respawn(
                &mut conn,
                &proto,
                &mut state,
                home,
                current,
                &mut point,
                RESPAWN_WORLD_SPAWN,
                GameMode::Survival,
                &mut teleport_acknowledgements,
                &feed,
                true,
            )
            .await
            .expect("the fixture protocol never errors");
        }
    }

    drop(client_end);
    let log = proto.0.lock().unwrap().clone();
    RespawnRun { reset: dimension_reset, point, log, feed }
}

/// The pre-anchor driver: no stored point, a source that only names its
/// dimension. Returns what `dimension_reset` ended up holding.
async fn drive_respawn(away_from_home: bool) -> Option<Vec3> {
    let home = DimensionOnly(crate::dimension::Dimension::Overworld);
    let current = DimensionOnly(if away_from_home {
        crate::dimension::Dimension::Nether
    } else {
        crate::dimension::Dimension::Overworld
    });
    drive_respawn_between(Respawn::Death, &home, &current, None)
        .await
        .reset
        .map(|reset| reset.target)
}

fn nether_anchor_point() -> RespawnPoint {
    RespawnPoint::block(BlockPos::new(0, 64, 0), crate::dimension::Dimension::Nether, 0.0)
}

fn overworld_bed_point() -> RespawnPoint {
    RespawnPoint::block(BlockPos::new(0, 64, 0), crate::dimension::Dimension::Overworld, 0.0)
}

use crate::dimension::Dimension as Dim;

/// Dying in the Nether with a charged anchor there respawns beside it, in the
/// Nether, spending one charge and playing the depletion sound.
#[tokio::test]
async fn a_nether_death_respawns_at_a_charged_anchor_in_the_nether() {
    let home = Arc::new(BlockWorld::flat(Dim::Overworld));
    let nether = Arc::new(BlockWorld::with_anchor(Dim::Nether, 3));
    home.link(&nether);
    let run = drive_respawn_between(Respawn::Death, &*home, &*nether, Some(nether_anchor_point())).await;
    // North of the anchor is first in the search order; the floor is the stone
    // at y = 63, so feet are at y = 64.
    let feet = Vec3::new(0.5, 64.0, -0.5);
    let reset = run.reset.expect("the view is rebuilt around the new spot");
    assert_eq!(reset.target, feet);
    assert_eq!(reset.route, crate::respawn::Route::Current, "the player stays in the Nether");
    assert_eq!(crate::respawn_anchor::charges(nether.block_state_id(0, 64, 0)), 2);
    assert_eq!(run.point, Some(nether_anchor_point()), "the point survives a charged respawn");
    assert_eq!(
        run.log,
        vec![
            "dimension minecraft:the_nether 0.5 64 -0.5".to_owned(),
            "sound minecraft:block.respawn_anchor.deplete".to_owned(),
        ],
        "a dimension frame for the Nether (not a home respawn), then the sound, and no event"
    );
    let published = run.feed.drain_all();
    assert_eq!(published.len(), 1, "one block change for the spent charge");
    assert_eq!((published[0].x, published[0].y, published[0].z), (0, 64, 0));
    assert_eq!(crate::respawn_anchor::charges(published[0].state), 2);
}

/// Dying in the overworld with a charged anchor in the Nether travels there.
/// The pair above is the control: the same anchor, a different death dimension.
#[tokio::test]
async fn an_overworld_death_respawns_at_a_nether_anchor_by_travelling() {
    let home = Arc::new(BlockWorld::flat(Dim::Overworld));
    let nether = Arc::new(BlockWorld::with_anchor(Dim::Nether, 3));
    home.link(&nether);
    let run = drive_respawn_between(Respawn::Death, &*home, &*home, Some(nether_anchor_point())).await;
    let reset = run.reset.expect("a dimension change must rebuild the view");
    assert_eq!(reset.target, Vec3::new(0.5, 64.0, -0.5));
    let sibling: Arc<dyn ChunkSource> = nether.clone();
    assert_eq!(reset.route, crate::respawn::Route::Sibling(sibling), "the Nether source itself");
    assert_eq!(crate::respawn_anchor::charges(nether.block_state_id(0, 64, 0)), 2, "spent in the Nether");
    assert_eq!(
        run.log,
        vec![
            "dimension minecraft:the_nether 0.5 64 -0.5".to_owned(),
            "sound minecraft:block.respawn_anchor.deplete".to_owned(),
        ]
    );
}

/// Dying in the Nether with a bed in the overworld returns home to the bed.
#[tokio::test]
async fn a_nether_death_respawns_at_an_overworld_bed() {
    let home = Arc::new(BlockWorld::with_bed(Dim::Overworld));
    let nether = Arc::new(BlockWorld::flat(Dim::Nether));
    home.link(&nether);
    let run = drive_respawn_between(Respawn::Death, &*home, &*nether, Some(overworld_bed_point())).await;
    // The first stand-up cell is one east of a north-facing bed.
    let reset = run.reset.expect("back home from the Nether");
    assert_eq!(reset.target, Vec3::new(1.5, 64.0, 0.5));
    assert_eq!(reset.route, crate::respawn::Route::Home);
    assert_eq!(run.log, vec!["respawn 1.5 64 0.5".to_owned()], "a death sends a respawn frame, no sound");
    assert_eq!(run.point, Some(overworld_bed_point()));
}

/// A bed in the dimension the player died in needs no reset at all.
#[tokio::test]
async fn an_overworld_death_at_an_overworld_bed_needs_no_reset() {
    let home = BlockWorld::with_bed(Dim::Overworld);
    let run = drive_respawn_between(Respawn::Death, &home, &home, Some(overworld_bed_point())).await;
    assert!(run.reset.is_none());
    assert_eq!(run.log, vec!["respawn 1.5 64 0.5".to_owned()]);
}

/// An empty anchor is a missing respawn block: the client is sent the
/// no-respawn-block event before the respawn, the point is cleared, and the
/// player goes home to the world spawn.
#[tokio::test]
async fn an_empty_anchor_sends_the_no_respawn_event_and_clears_the_point() {
    let home = Arc::new(BlockWorld::flat(Dim::Overworld));
    let nether = Arc::new(BlockWorld::with_anchor(Dim::Nether, 0));
    home.link(&nether);
    let run = drive_respawn_between(Respawn::Death, &*home, &*nether, Some(nether_anchor_point())).await;
    let reset = run.reset.expect("home from the Nether");
    assert_eq!(reset.target, RESPAWN_WORLD_SPAWN);
    assert_eq!(reset.route, crate::respawn::Route::Home);
    assert_eq!(run.point, None, "the unusable point is cleared");
    assert_eq!(
        run.log,
        vec!["event 0 0".to_owned(), format!("respawn {} {} {}", 11, 71, -4)],
        "the event precedes the respawn frame, and there is no depletion sound"
    );
    assert_eq!(crate::respawn_anchor::charges(nether.block_state_id(0, 64, 0)), 0);
    assert!(run.feed.drain_all().is_empty(), "nothing was spent");
}

/// A removed anchor behaves like an empty one.
#[tokio::test]
async fn a_missing_anchor_sends_the_no_respawn_event() {
    let home = Arc::new(BlockWorld::flat(Dim::Overworld));
    let nether = Arc::new(BlockWorld::with_anchor(Dim::Nether, 2));
    nether.set_block(0, 64, 0, crate::chunk::air_state());
    home.link(&nether);
    let run = drive_respawn_between(Respawn::Death, &*home, &*home, Some(nether_anchor_point())).await;
    assert_eq!(run.point, None);
    assert_eq!(run.log.first().map(String::as_str), Some("event 0 0"));
}

/// A point in a dimension the world cannot reach falls back to the world spawn
/// silently and is kept.
#[tokio::test]
async fn a_point_in_an_unreachable_dimension_falls_back_quietly() {
    let home = BlockWorld::flat(Dim::Overworld);
    let run = drive_respawn_between(Respawn::Death, &home, &home, Some(nether_anchor_point())).await;
    assert!(run.reset.is_none());
    assert_eq!(run.point, Some(nether_anchor_point()));
    assert_eq!(run.log, vec![format!("respawn {} {} {}", 11, 71, -4)]);
}

/// A forced point respawns at its position even with no block there, and a
/// blocked cell makes it missing.
#[tokio::test]
async fn a_forced_point_respawns_at_its_position_when_the_cells_are_free() {
    let home = BlockWorld::flat(Dim::Overworld);
    let point = RespawnPoint::forced(BlockPos::new(2, 64, 2), Dim::Overworld, 90.0, 10.0);
    let run = drive_respawn_between(Respawn::Death, &home, &home, Some(point)).await;
    assert_eq!(run.log, vec![format!("respawn {} {} {}", 2.5, 64.1, 2.5)]);
    assert_eq!(run.point, Some(point));
    // Control: stone in the cell makes the same point unusable.
    home.set_block(2, 64, 2, StateId::from_state_str("minecraft:stone").unwrap());
    let blocked = drive_respawn_between(Respawn::Death, &home, &home, Some(point)).await;
    assert_eq!(blocked.point, None);
    assert_eq!(blocked.log.first().map(String::as_str), Some("event 0 0"));
}

/// A forced anchor works with no charge and costs none, and still plays the
/// depletion sound.
#[tokio::test]
async fn a_forced_anchor_needs_no_charge_and_spends_none() {
    let home = Arc::new(BlockWorld::flat(Dim::Overworld));
    let nether = Arc::new(BlockWorld::with_anchor(Dim::Nether, 0));
    home.link(&nether);
    let point = RespawnPoint::forced(BlockPos::new(0, 64, 0), Dim::Nether, 0.0, 0.0);
    let run = drive_respawn_between(Respawn::Death, &*home, &*nether, Some(point)).await;
    assert_eq!(crate::respawn_anchor::charges(nether.block_state_id(0, 64, 0)), 0);
    assert!(run.log.contains(&"sound minecraft:block.respawn_anchor.deplete".to_owned()));
    assert!(run.feed.drain_all().is_empty());
}

/// Leaving the End reads the point without using it. A Nether anchor sends the
/// player to the Nether, keeps its charges and plays no sound.
#[tokio::test]
async fn an_end_exit_travels_to_a_nether_anchor_without_spending_it() {
    let home = Arc::new(BlockWorld::flat(Dim::Overworld));
    let nether = Arc::new(BlockWorld::with_anchor(Dim::Nether, 3));
    home.link(&nether);
    let end = BlockWorld::flat(Dim::End);
    let run = drive_respawn_between(Respawn::EndExit, &*home, &end, Some(nether_anchor_point())).await;
    let reset = run.reset.expect("the exit always rebuilds the view");
    assert_eq!(reset.target, Vec3::new(0.5, 64.0, -0.5));
    let sibling: Arc<dyn ChunkSource> = nether.clone();
    assert_eq!(reset.route, crate::respawn::Route::Sibling(sibling));
    assert_eq!(crate::respawn_anchor::charges(nether.block_state_id(0, 64, 0)), 3, "an exit spends nothing");
    assert_eq!(run.log, vec!["dimension minecraft:the_nether 0.5 64 -0.5".to_owned()]);
    assert!(run.feed.drain_all().is_empty());
}

/// Leaving the End with an overworld bed goes home to the bed, and with a point
/// that is no longer usable goes to the world spawn with the event and a
/// cleared point, as a death does.
#[tokio::test]
async fn an_end_exit_goes_home_to_a_bed_or_reports_a_missing_block() {
    let home = BlockWorld::with_bed(Dim::Overworld);
    let end = BlockWorld::flat(Dim::End);
    let run = drive_respawn_between(Respawn::EndExit, &home, &end, Some(overworld_bed_point())).await;
    assert_eq!(run.log, vec!["dimension minecraft:overworld 1.5 64 0.5".to_owned()]);
    assert_eq!(run.reset.expect("reset").route, crate::respawn::Route::Home);
    home.set_block(0, 64, 0, crate::chunk::air_state());
    let missing = drive_respawn_between(Respawn::EndExit, &home, &end, Some(overworld_bed_point())).await;
    assert_eq!(missing.point, None);
    assert_eq!(
        missing.log,
        vec!["event 0 0".to_owned(), "dimension minecraft:overworld 11 71 -4".to_owned()]
    );
}

/// What a driven anchor click left behind.
struct UseRun {
    log: Vec<String>,
    respawn: Option<RespawnPoint>,
    inventory: PlayerInventory,
    feed: BlockTickFeed,
    mobs: MobHandle,
}

/// Right-clicks the anchor at (0, 64, 0) of `world` through the real
/// `apply_use_item_on`, holding `main` in hotbar slot 0 and `off` in the off
/// hand (item path, count).
async fn click_anchor(
    world: &BlockWorld,
    main: Option<(&str, u32)>,
    off: Option<(&str, u32)>,
    sneaking: bool,
    game_mode: GameMode,
    respawn: Option<RespawnPoint>,
) -> UseRun {
    let (client_end, server_end) = lodestone_net::memory_pair();
    let mut conn = Connection::new(server_end);
    let mut state = State::Play;
    let proto = RespawnOnlyProto::default();
    let mut inventory = PlayerInventory::new();
    if let Some((item, count)) = main {
        inventory.set_native(0, Some(ItemStack::new(format!("minecraft:{item}").parse().unwrap(), count)));
    }
    if let Some((item, count)) = off {
        inventory.set_native(OFFHAND_NATIVE, Some(ItemStack::new(format!("minecraft:{item}").parse().unwrap(), count)));
    }
    let mut respawn = respawn;
    let feed = BlockTickFeed::default();
    let mobs = MobHandle::new(crate::ChunkWorld::new(0, 256));
    let mut next_window_id = 1;
    let mut open_container = None;
    let mut container_sync = ContainerSync::default();
    let mut bone_meal_rng = SpawnRng::new(1);
    apply_use_item_on(
        &mut conn,
        &proto,
        world,
        &mut state,
        None,
        BlockPos::new(0, 64, 0),
        BlockFace::Up,
        Vec3f::new(0.5, 1.0, 0.5),
        None,
        &mut respawn,
        None,
        None,
        sneaking,
        Uuid::nil(),
        &mut inventory,
        &BlockEntityHandle::default(),
        &mut next_window_id,
        &mut open_container,
        &mut container_sync,
        &mobs,
        0.5,
        &feed,
        &SleepVote::default(),
        1,
        &mut bone_meal_rng,
        lodestone_model::Difficulty::Normal,
        game_mode,
        0,
        0,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    )
    .await
    .expect("the fixture protocol never errors");
    drop(client_end);
    let log = proto.0.lock().unwrap().clone();
    UseRun { log, respawn, inventory, feed, mobs }
}

fn sound_names(feed: &BlockTickFeed) -> Vec<String> {
    feed.drain_effects_for(Uuid::nil())
        .into_iter()
        .filter_map(|effect| match effect {
            crate::effects::WorldEffect::Sound { sound, .. } => Some(sound),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn glowstone_charges_an_anchor_one_step_and_is_consumed() {
    let world = BlockWorld::with_anchor(crate::dimension::Dimension::Nether, 1);
    let run = click_anchor(&world, Some(("glowstone", 2)), None, false, GameMode::Survival, None).await;
    assert_eq!(crate::respawn_anchor::charges(world.block_state_id(0, 64, 0)), 2);
    assert_eq!(run.inventory.native(0).map(|stack| stack.count), Some(1), "one block spent");
    assert_eq!(
        run.log,
        vec!["block 0 64 0 charges=2".to_owned(), "slot 36 count=1".to_owned()],
        "the client is told the block and the new hotbar count"
    );
    assert_eq!(sound_names(&run.feed), vec!["minecraft:block.respawn_anchor.charge".to_owned()]);
    assert_eq!(run.respawn, None, "charging does not set the spawn");
}

/// The charge ceiling is the control for the test above: at 4 the same
/// click no longer charges, spending nothing.
#[tokio::test]
async fn a_full_anchor_takes_no_more_glowstone() {
    let world = BlockWorld::with_anchor(crate::dimension::Dimension::Nether, 4);
    let run = click_anchor(&world, Some(("glowstone", 2)), None, false, GameMode::Survival, None).await;
    assert_eq!(crate::respawn_anchor::charges(world.block_state_id(0, 64, 0)), 4);
    assert_eq!(run.inventory.native(0).map(|stack| stack.count), Some(2));
    assert!(sound_names(&run.feed).iter().all(|name| !name.ends_with(".charge")));
}

#[tokio::test]
async fn creative_charging_keeps_the_stack() {
    let world = BlockWorld::with_anchor(crate::dimension::Dimension::Nether, 0);
    let run = click_anchor(&world, Some(("glowstone", 1)), None, false, GameMode::Creative, None).await;
    assert_eq!(crate::respawn_anchor::charges(world.block_state_id(0, 64, 0)), 1);
    assert_eq!(run.inventory.native(0).map(|stack| stack.count), Some(1));
}

#[tokio::test]
async fn sneaking_with_glowstone_skips_the_anchor() {
    let world = BlockWorld::with_anchor(crate::dimension::Dimension::Nether, 1);
    // A solid roof, so the skipped use falls through to a placement that has
    // nowhere to go rather than building a glowstone block.
    world.set_block(0, 65, 0, StateId::from_state_str("minecraft:stone").unwrap());
    let run = click_anchor(&world, Some(("glowstone", 2)), None, true, GameMode::Survival, None).await;
    assert_eq!(crate::respawn_anchor::charges(world.block_state_id(0, 64, 0)), 1);
    assert_eq!(run.inventory.native(0).map(|stack| stack.count), Some(2));
}

#[tokio::test]
async fn glowstone_in_the_off_hand_charges_when_the_main_hand_is_empty() {
    let world = BlockWorld::with_anchor(crate::dimension::Dimension::Nether, 0);
    let run = click_anchor(&world, None, Some(("glowstone", 3)), false, GameMode::Survival, None).await;
    // This drives the main-hand click, which defers to the off-hand click
    // the client sends next, so nothing changes on this call.
    assert_eq!(crate::respawn_anchor::charges(world.block_state_id(0, 64, 0)), 0);
    assert!(run.log.is_empty());
}

#[tokio::test]
async fn a_charged_anchor_in_the_nether_sets_the_respawn_point_once() {
    let world = BlockWorld::with_anchor(crate::dimension::Dimension::Nether, 2);
    let run = click_anchor(&world, None, None, false, GameMode::Survival, None).await;
    let point = RespawnPoint::block(BlockPos::new(0, 64, 0), crate::dimension::Dimension::Nether, 0.0);
    assert_eq!(run.respawn, Some(point));
    assert_eq!(run.log, vec!["chat Respawn point set".to_owned()]);
    assert_eq!(sound_names(&run.feed), vec!["minecraft:block.respawn_anchor.set_spawn".to_owned()]);
    assert_eq!(crate::respawn_anchor::charges(world.block_state_id(0, 64, 0)), 2, "using it spends nothing");
    // The same click again changes nothing and says nothing.
    let again = click_anchor(&world, None, None, false, GameMode::Survival, Some(point)).await;
    assert!(again.log.is_empty());
    assert!(sound_names(&again.feed).is_empty());
}

#[tokio::test]
async fn an_empty_anchor_does_not_set_the_respawn_point() {
    let world = BlockWorld::with_anchor(crate::dimension::Dimension::Nether, 0);
    let run = click_anchor(&world, None, None, false, GameMode::Survival, None).await;
    assert_eq!(run.respawn, None);
    assert!(sound_names(&run.feed).is_empty());
}

/// Used outside the Nether, a charged anchor is removed and queues a power-5
/// blast that sets fires; the Nether case above is the control that the same
/// click does not blast where anchors work.
#[tokio::test]
async fn a_charged_anchor_outside_the_nether_explodes() {
    let world = BlockWorld::with_anchor(crate::dimension::Dimension::Overworld, 1);
    let run = click_anchor(&world, None, None, false, GameMode::Survival, None).await;
    assert!(!crate::respawn_anchor::is_anchor(world.block_state_id(0, 64, 0)), "the anchor is gone");
    assert_eq!(run.respawn, None);
    let blasts = run.mobs.with(|sim| sim.take_detonations());
    assert_eq!(blasts.len(), 1);
    assert_eq!(blasts[0].centre, Vec3::new(0.5, 64.5, 0.5));
    assert!((blasts[0].radius - 5.0).abs() < f32::EPSILON);
    assert!(blasts[0].fire, "an anchor blast sets fires");
}

/// Charging an anchor wakes a comparator reading it: the comparator is scheduled
/// to refresh, which only happens when the input it reads is the anchor's level.
#[tokio::test]
async fn charging_an_anchor_schedules_the_comparator_that_reads_it() {
    let world = BlockWorld::with_anchor(crate::dimension::Dimension::Nether, 1);
    let comparator = StateId::from_state_str("minecraft:comparator[facing=west,mode=compare,powered=false]").unwrap();
    world.set_block(1, 64, 0, comparator);
    let run = click_anchor(&world, Some(("glowstone", 1)), None, false, GameMode::Survival, None).await;
    assert_eq!(crate::respawn_anchor::charges(world.block_state_id(0, 64, 0)), 2);
    let scheduled = run.feed.drain_scheduled_ticks();
    assert!(
        scheduled.iter().any(|tick| tick.pos == (1, 64, 0) && tick.kind == crate::scheduled_tick::ScheduledTickKind::Comparator),
        "the comparator must be scheduled, got {scheduled:?}"
    );
}

/// An anchor blast in water leaves the terrain alone and sets no fire, but is
/// still a blast. Water above and a beside source smother it; water below and a
/// one-deep trickle beside it do not.
#[tokio::test]
async fn an_anchor_blast_in_water_destroys_nothing() {
    let water = |level: u8| StateId::from_state_str(&format!("minecraft:water[level={level}]")).unwrap();
    for (name, at, level, smothered) in [
        ("above", (0, 65, 0), 0, true),
        ("a source beside", (1, 64, 0), 0, true),
        ("a deep flow beside", (0, 64, -1), 3, true),
        ("a thin flow beside", (-1, 64, 0), 7, false),
        ("below", (0, 63, 0), 0, false),
    ] {
        let world = BlockWorld::with_anchor(crate::dimension::Dimension::Overworld, 1);
        world.set_block(at.0, at.1, at.2, water(level));
        let run = click_anchor(&world, None, None, false, GameMode::Survival, None).await;
        let blasts = run.mobs.with(|sim| sim.take_detonations());
        assert_eq!(blasts.len(), 1, "{name}: the explosion is still published");
        assert_eq!(blasts[0].destroys_blocks, !smothered, "{name}");
        assert_eq!(blasts[0].fire, !smothered, "{name}");
    }
}

/// **Death away from home returns a reset request.** A death away from home must ask the caller to run the same
/// dimension reset a portal trip home runs — carrying the resolved respawn
/// position (here the world spawn, since no bed is set), or the connection
/// loop never re-centres `view`/`join_stream` and the client sits at a
/// correctly-labelled position with no terrain ever streamed to it.
#[tokio::test]
async fn a_death_away_from_home_asks_for_a_dimension_reset() {
    let reset = drive_respawn(true).await;
    assert_eq!(
        reset,
        Some(Vec3::new(11.0, 71.0, -4.0)),
        "a death in the Nether must signal a reset carrying the resolved respawn position"
    );
}

/// **The control.** An ordinary death *at* home must not trip the same
/// signal, or every respawn would pay the forget-chunk/rebuild-join-stream
/// cost this exists to avoid on the common path. Without the
/// `away_from_home` gate this assertion fails identically to the one above
/// passing — proving the gate is load-bearing, not decorative.
#[tokio::test]
async fn a_death_at_home_does_not_ask_for_a_dimension_reset() {
    let reset = drive_respawn(false).await;
    assert_eq!(
        reset, None,
        "a same-dimension death must not signal a reset"
    );
}
