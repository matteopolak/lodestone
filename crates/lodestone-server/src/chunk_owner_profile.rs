//! Deterministic, populated workload for profiling the serial chunk-owner
//! hand-off boundaries in the integrated tick loop.

use lodestone_model::{BlockPos, ItemStack, ResourceKey, Vec3};

use crate::{
    BlockEntity, ChunkColumn, ChunkSource, Furnace, FurnaceKind, IntegratedServer, ScheduledTickQueue,
    TickPriority, TickStats,
};
use crate::protocol::ServerProtocol;

/// Stable name written by the criterion and Samply entry points.
pub const SCENE_NAME: &str = "chunk-owner-mixed-8";
/// Eight owners, each with one furnace and one scheduled block/fluid pair.
pub const OWNER_COUNT: usize = 8;
/// Ambient producers spread across the same eight chunk owners.
pub const AMBIENT_MOB_COUNT: usize = 64;

const FLOOR_TOP: i32 = 4;
const TICK_PERIOD: std::time::Duration = std::time::Duration::from_millis(50);
const RESEED_POLLS: usize = 100_000;

/// The work witnesses from one finite run of [`SCENE_NAME`].
#[derive(Debug, Clone, Copy)]
pub struct ChunkOwnerProfileReport {
    /// Number of driven 50ms game ticks.
    pub ticks: u64,
    /// The integrated loop's phase and owner-boundary counters.
    pub stats: TickStats,
}

/// Runs the named workload. With the `profile-harness` feature, a paused
/// runtime makes its tick count an input rather than a function of host speed;
/// without it, the same finite sequence uses the ordinary server timer.
/// This is intentionally a profiling fixture, not a duration gate.
pub fn run<P: ServerProtocol + 'static>(protocol: P, ticks: u64) -> ChunkOwnerProfileReport {
    let mut builder = tokio::runtime::Builder::new_current_thread();
    builder.enable_all();
    #[cfg(feature = "profile-harness")]
    builder.start_paused(true);
    let runtime = builder.build().expect("a current-thread runtime");
    runtime.block_on(async move {
        let (server, _client) = IntegratedServer::open_in_memory_with_mobs(
            protocol,
            ProfileWorld,
            (-1..=2, 0..=1),
            (8, 8),
            0,
            3,
        );
        wait_for_reseed(&server).await;
        seed_block_entities(&server);
        seed_ambient_mobs(&server);
        seed_scheduled_ticks(&server);
        tokio::task::yield_now().await;

        for _ in 0..ticks {
            #[cfg(feature = "profile-harness")]
            tokio::time::advance(TICK_PERIOD).await;
            #[cfg(not(feature = "profile-harness"))]
            tokio::time::sleep(TICK_PERIOD).await;
            tokio::task::yield_now().await;
        }
        let stats = server.tick_stats().expect("the profile scene starts the live tick loop");
        server.shutdown().await;
        ChunkOwnerProfileReport { ticks, stats }
    })
}

async fn wait_for_reseed(server: &IntegratedServer) {
    let mobs = server.mobs().expect("the profile scene exposes its mob simulation");
    for _ in 0..RESEED_POLLS {
        if mobs.with(|sim| sim.next_id() >= 1000) {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the profile scene did not finish its deterministic mob reseed");
}

fn owner_positions() -> impl Iterator<Item = BlockPos> {
    (0..OWNER_COUNT).map(|index| {
        let cx = index as i32 % 4 - 1;
        let cz = index as i32 / 4;
        BlockPos::new(cx * 16 + 8, FLOOR_TOP, cz * 16 + 8)
    })
}

fn seed_block_entities(server: &IntegratedServer) {
    let handle = server.block_entities().expect("the profile scene owns a block-entity registry");
    handle.with(|registry| {
        for pos in owner_positions() {
            let mut furnace = Furnace::new(FurnaceKind::Furnace);
            furnace.set_fuel(Some(ItemStack::new("minecraft:coal".parse().expect("valid fuel"), 1)));
            furnace.set_input(Some(ItemStack::new("minecraft:iron_ore".parse().expect("valid input"), 1)));
            registry.insert(pos, BlockEntity::Furnace(furnace));
        }
    });
}

fn seed_ambient_mobs(server: &IntegratedServer) {
    let cow: ResourceKey = "minecraft:cow".parse().expect("valid cow key");
    for index in 0..AMBIENT_MOB_COUNT {
        let owner = index % OWNER_COUNT;
        let cx = owner as i32 % 4 - 1;
        let cz = owner as i32 / 4;
        let lane = index / OWNER_COUNT;
        server
            .spawn_mob(
                cow.clone(),
                Vec3::new(
                    (cx * 16 + 2 + (lane % 4) as i32 * 3) as f64,
                    FLOOR_TOP as f64,
                    (cz * 16 + 2 + (lane / 4) as i32 * 3) as f64,
                ),
            )
            .expect("the profile scene accepts its ambient mob");
    }
}

fn seed_scheduled_ticks(server: &IntegratedServer) {
    let mut pending = ScheduledTickQueue::new();
    for pos in owner_positions() {
        assert!(pending.schedule(
            (pos.x + 1, pos.y, pos.z),
            "redstone:repeater".to_owned(),
            1,
            TickPriority::Normal,
        ));
        assert!(pending.schedule(
            (pos.x - 1, pos.y, pos.z),
            "lodestone:fluid".to_owned(),
            1,
            TickPriority::Normal,
        ));
    }
    server
        .block_ticks()
        .expect("the profile scene owns a scheduled-tick feed")
        .request_scheduled_ticks(pending.drain_due(u64::MAX, usize::MAX));
}

/// A fixed 4x2 owner field. Columns have a floor, one active furnace state,
/// one repeater state and one water source so the scheduled queues execute the
/// same production readers and writers as a running integrated world.
struct ProfileWorld;

impl ProfileWorld {
    fn build_column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(0, 64);
        for y in 0..FLOOR_TOP {
            for z in 0..16 {
                for x in 0..16 {
                    column.set_block(x, y, z, "minecraft:stone");
                }
            }
        }
        if (-1..=2).contains(&cx) && (0..=1).contains(&cz) {
            column.set_block(8, FLOOR_TOP, 8, "minecraft:furnace[facing=north,lit=false]");
            column.set_block(9, FLOOR_TOP, 8, "minecraft:repeater[delay=1,facing=north,locked=false,powered=true]");
            column.set_block(7, FLOOR_TOP, 8, "minecraft:water[level=0]");
        }
        column
    }
}

impl ChunkSource for ProfileWorld {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.build_column(cx, cz)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let local_x = x.rem_euclid(16);
        let local_z = z.rem_euclid(16);
        self.build_column(cx, cz).block_state(local_x, y, local_z).to_owned()
    }

    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}
