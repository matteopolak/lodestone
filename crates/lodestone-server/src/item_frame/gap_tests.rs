//! Damage, summon and comparator rules for item frames. Every expected value is
//! worked from the reference rules by hand, not read back from the code: a
//! blast hurts a frame whose centre lies within twice its radius whatever
//! stands between, an explosion breaks a frame where a projectile or fire only
//! pops its item, a fixed frame yields to nothing but a creative player, and a
//! comparator reads `rotation + 1` from a frame holding an item and 0 from an
//! empty one, through exactly one conductor.

use std::sync::{Arc, Mutex};

use lodestone_core::Nbt;
use lodestone_data::block::Block;
use lodestone_data::block_states::{StateId, air_state};
use lodestone_entity::projectile::Projectile;
use lodestone_model::{BlockFace, BlockPos, ItemStack, ResourceKey, Vec3};

use crate::block_entities::BlockEntityHandle;
use crate::chunk::{ChunkColumn, ChunkSource};
use crate::mobs::{ChunkWorld, FrameDamage, FrameHit, LiveMobSource, MobHandle, MobSim};
use crate::neighbor_update::Direction;
use crate::redstone::RedstoneLookup;
use crate::tick::{BlockTickFeed, ExplosionFeed, TICK_PERIOD, TickClock, run_tick_loop};
use crate::{redstone, redstone_diode};

const CELL: BlockPos = BlockPos { x: 6, y: 65, z: 3 };

fn stack(item: &str) -> ItemStack {
    ItemStack::new(item.parse::<ResourceKey>().unwrap(), 1)
}

fn rk(s: &str) -> ResourceKey {
    s.parse().unwrap()
}

fn never(_: i32) -> bool {
    false
}

/// An East-facing frame in [`CELL`] holding a diamond.
fn full_frame(sim: &mut MobSim<'_>) -> i32 {
    let id = sim.spawn_item_frame(CELL, BlockFace::East, false);
    sim.interact_frame(id, Some(&stack("minecraft:diamond")), &never);
    id
}

fn dropped(sim: &MobSim<'_>) -> Vec<(String, u8)> {
    let mut items = sim.dropped_items();
    items.sort();
    items
}

fn set_flag(sim: &mut MobSim<'_>, id: i32, name: &str) {
    let mut saved = sim.saved_entities().into_iter().find(|s| s.id.path() == "item_frame").unwrap();
    // Rebuild the frame from its record with the flag set, as a save would carry it.
    sim.kill_frame(id, false);
    saved.extra.retain(|(k, _)| k != name);
    saved.extra.push((name.to_owned(), Nbt::Byte(1)));
    sim.summon_saved(&saved).expect("the frame restores");
}

fn only_frame(sim: &MobSim<'_>) -> i32 {
    sim.command_entities().into_iter().find(|e| e.entity_type.path().ends_with("item_frame")).expect("one frame").entity_id
}

// -- Explosions ---------------------------------------------------------------

/// The frame's box centre is (6.03125, 65.5, 3.5). A power-4 blast reaches
/// twice its radius, 8 blocks: a blast 7.96875 blocks east of the centre
/// reaches it, one 8.03125 blocks away does not.
#[test]
fn a_blast_breaks_a_frame_within_twice_its_radius_and_spares_one_beyond() {
    let world = ChunkWorld::new(-64, 384);
    let mut sim = MobSim::new(&world);
    let id = full_frame(&mut sim);
    sim.queue_blast(Vec3::new(6.03125 + 8.03125, 65.5, 3.5), 4.0, false, true);
    assert!(sim.is_item_frame(id), "8.03125 blocks is beyond 2 * 4");
    assert!(dropped(&sim).is_empty());

    sim.queue_blast(Vec3::new(6.03125 + 7.96875, 65.5, 3.5), 4.0, false, true);
    assert!(!sim.is_item_frame(id), "7.96875 blocks is inside 2 * 4");
    // The explosion breaks the frame instead of popping the item: both drop.
    assert_eq!(dropped(&sim), vec![("minecraft:diamond".to_owned(), 1), ("minecraft:item_frame".to_owned(), 1)]);
    let events = sim.take_frame_events();
    assert!(matches!(events.as_slice(), [FrameHit::Broken(_)]), "{events:?}");
}

/// The wall is solid stone between the blast and the frame; the reference
/// hurts blocklike entities by distance alone.
#[test]
fn a_wall_does_not_shield_a_frame_from_a_blast() {
    let mut world = ChunkWorld::new(-64, 384);
    for x in 7..=9 {
        for y in 60..=70 {
            for z in -2..=8 {
                world.set_solid(x, y, z, true);
            }
        }
    }
    let mut sim = MobSim::new(&world);
    let id = full_frame(&mut sim);
    sim.queue_blast(Vec3::new(12.0, 65.5, 3.5), 4.0, false, true);
    assert!(!sim.is_item_frame(id));
}

#[test]
fn a_blast_that_breaks_no_blocks_reaches_frames_only_while_mobs_may_grief() {
    let world = ChunkWorld::new(-64, 384);
    let blast = Vec3::new(8.0, 65.5, 3.5);

    let mut sim = MobSim::new(&world);
    let id = full_frame(&mut sim);
    sim.set_mob_griefing(false);
    sim.queue_blast(blast, 4.0, false, false);
    assert!(sim.is_item_frame(id), "no griefing and no block damage: frames are left alone");
    sim.queue_blast(blast, 4.0, false, true);
    assert!(!sim.is_item_frame(id), "a block-breaking blast still reaches them");

    let mut sim = MobSim::new(&world);
    let id = full_frame(&mut sim);
    sim.queue_blast(blast, 4.0, false, false);
    assert!(!sim.is_item_frame(id), "with griefing on, a blast that keeps terrain still breaks frames");
}

#[test]
fn a_mobs_blast_is_refused_while_mob_griefing_is_off() {
    let world = ChunkWorld::new(-64, 384);
    let mut sim = MobSim::new(&world);
    let id = full_frame(&mut sim);
    sim.set_mob_griefing(false);
    sim.explode_by(Vec3::new(8.0, 65.5, 3.5), 3.0, Default::default(), true, true);
    assert!(sim.is_item_frame(id));
    sim.set_mob_griefing(true);
    sim.explode_by(Vec3::new(8.0, 65.5, 3.5), 3.0, Default::default(), true, true);
    assert!(!sim.is_item_frame(id));
}

#[test]
fn fixed_and_invulnerable_frames_ignore_blasts_but_not_creative_players() {
    let world = ChunkWorld::new(-64, 384);
    for flag in ["Fixed", "Invulnerable"] {
        let mut sim = MobSim::new(&world);
        let id = full_frame(&mut sim);
        set_flag(&mut sim, id, flag);
        let id = only_frame(&sim);
        sim.queue_blast(Vec3::new(8.0, 65.5, 3.5), 4.0, false, true);
        assert!(sim.is_item_frame(id), "{flag} frame survives a blast");
        for damage in [FrameDamage::Projectile { by_mob: false, creative_shooter: false }] {
            assert_eq!(sim.hurt_frame_with(id, damage), Some(FrameHit::Ignored), "{flag} {damage:?}");
        }
        assert!(
            matches!(sim.hurt_frame_with(id, FrameDamage::Player { creative: true }), Some(FrameHit::Broken(_)) | Some(FrameHit::ItemPopped { .. })),
            "{flag}: creative bypasses"
        );
    }
}

// -- Projectiles --------------------------------------------------------------

fn wall_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    world.set_solid(5, 65, 3, true);
    world
}

/// An arrow flying west at 3 blocks per tick from x = 9 reaches the frame's
/// slab at x in [6, 6.0625], well before the wall block behind it.
fn fire(sim: &mut MobSim<'_>, path: &str) {
    let velocity = Vec3::new(-3.0, 0.0, 0.0);
    let from = Vec3::new(9.0, 65.5, 3.5);
    let projectile = if path == "snowball" { Projectile::snowball(from, velocity) } else { Projectile::arrow(from, velocity) };
    sim.spawn_projectile(rk(&format!("minecraft:{path}")), projectile);
    sim.tick();
}

#[test]
fn an_arrow_pops_the_item_then_breaks_the_frame() {
    let world = wall_world();
    let mut sim = MobSim::new(&world);
    let id = full_frame(&mut sim);
    fire(&mut sim, "arrow");
    assert_eq!(sim.projectile_count(), 0, "the arrow is spent on the frame");
    assert!(sim.is_item_frame(id), "the first hit only pops the item");
    assert_eq!(dropped(&sim), vec![("minecraft:diamond".to_owned(), 1)]);
    assert!(sim.frame_state(id).unwrap().2.is_none());

    fire(&mut sim, "arrow");
    assert!(!sim.is_item_frame(id), "the second hit breaks the empty frame");
    assert_eq!(dropped(&sim), vec![("minecraft:diamond".to_owned(), 1), ("minecraft:item_frame".to_owned(), 1)]);
    let events = sim.take_frame_events();
    assert!(matches!(events.as_slice(), [FrameHit::ItemPopped { .. }, FrameHit::Broken(_)]), "{events:?}");
}

/// A snowball deals no damage to anything but a blaze, yet it still knocks the
/// item out of a frame.
#[test]
fn a_snowball_deals_no_damage_and_still_pops_the_item() {
    let world = wall_world();
    let mut sim = MobSim::new(&world);
    let id = full_frame(&mut sim);
    fire(&mut sim, "snowball");
    assert!(sim.is_item_frame(id));
    assert_eq!(dropped(&sim), vec![("minecraft:diamond".to_owned(), 1)]);
}

#[test]
fn a_fixed_frame_takes_a_hit_without_losing_its_item() {
    let world = wall_world();
    let mut sim = MobSim::new(&world);
    let id = full_frame(&mut sim);
    set_flag(&mut sim, id, "Fixed");
    let id = only_frame(&sim);
    fire(&mut sim, "arrow");
    assert!(sim.is_item_frame(id));
    assert!(sim.frame_state(id).unwrap().2.is_some(), "the diamond stays");
    assert!(dropped(&sim).is_empty());
}

#[test]
fn a_mobs_arrow_is_refused_while_mob_griefing_is_off() {
    let world = wall_world();
    let mut sim = MobSim::new(&world);
    let id = full_frame(&mut sim);
    let cow = sim.spawn_species(rk("minecraft:cow"), Vec3::new(20.0, 65.0, 20.0)).id();
    sim.set_mob_griefing(false);
    let velocity = Vec3::new(-3.0, 0.0, 0.0);
    sim.spawn_projectile_from(rk("minecraft:arrow"), Projectile::arrow(Vec3::new(9.0, 65.5, 3.5), velocity), Some(cow));
    sim.tick();
    assert!(sim.frame_state(id).unwrap().2.is_some(), "the cow-fired arrow did nothing");
    sim.set_mob_griefing(true);
    sim.spawn_projectile_from(rk("minecraft:arrow"), Projectile::arrow(Vec3::new(9.0, 65.5, 3.5), velocity), Some(cow));
    sim.tick();
    assert!(sim.frame_state(id).unwrap().2.is_none(), "with griefing on it pops the item");
}

// -- Comparator input ------------------------------------------------------------

struct Lookup {
    states: Vec<(BlockPos, StateId)>,
    frames: Vec<(BlockPos, Direction, u8)>,
}

impl RedstoneLookup for Lookup {
    fn state_at(&self, pos: BlockPos) -> StateId {
        self.states.iter().find(|(p, _)| *p == pos).map_or_else(air_state, |(_, s)| *s)
    }
    fn comparator_output(&self, _pos: BlockPos) -> u8 {
        0
    }
    fn entity_analog_output(&self, cell: BlockPos, facing: Direction) -> Option<u8> {
        self.frames.iter().find(|(c, f, _)| *c == cell && *f == facing).map(|(_, _, s)| *s)
    }
}

#[test]
fn a_comparator_reads_a_frame_only_through_a_conductor_and_only_one_facing_away() {
    let at = BlockPos::new(0, 65, 0);
    let stone = Block::Stone.default_state();
    let lookup = |frames| Lookup { states: vec![(BlockPos::new(1, 65, 0), stone)], frames };
    let frame_cell = BlockPos::new(2, 65, 0);

    // A frame holding an item with rotation 4 outputs 5.
    assert_eq!(redstone::comparator_input_signal(&lookup(vec![(frame_cell, Direction::East, 5)]), at, Direction::East), 5);
    // An empty frame outputs 0 and replaces whatever else the input read.
    assert_eq!(redstone::comparator_input_signal(&lookup(vec![(frame_cell, Direction::East, 0)]), at, Direction::East), 0);
    // A frame facing the comparator is not the one hung on the conductor.
    assert_eq!(redstone::comparator_input_signal(&lookup(vec![(frame_cell, Direction::West, 5)]), at, Direction::East), 0);
    // Without a conductor between them there is nothing to read through.
    let open = Lookup { states: vec![], frames: vec![(frame_cell, Direction::East, 5)] };
    assert_eq!(redstone::comparator_input_signal(&open, at, Direction::East), 0);
    // Directly adjacent frames are never read: the frame must be one cell past the conductor.
    let adjacent = Lookup { states: vec![], frames: vec![(BlockPos::new(1, 65, 0), Direction::East, 5)] };
    assert_eq!(redstone::comparator_input_signal(&adjacent, at, Direction::East), 0);
}

// -- The tick loop ---------------------------------------------------------------

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const ROW_Z: i32 = 8;
const COMPARATOR: BlockPos = BlockPos { x: 3, y: 1, z: ROW_Z };

struct Rig(Mutex<ChunkColumn>);

impl Rig {
    fn new() -> Arc<Self> {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for x in 0..16 {
            for z in 0..16 {
                column.set_block_id(x, 0, z, Block::Stone.default_state());
            }
        }
        // Comparator reading east through the stone at x = 4; the frame hangs in
        // x = 5, on that stone's east face; dust takes the output on the west.
        column.set_block_id(4, 1, ROW_Z, Block::Stone.default_state());
        column.set_block_id(
            3,
            1,
            ROW_Z,
            redstone_diode::set_comparator_state(Direction::East, false, false, 0).state,
        );
        Arc::new(Self(Mutex::new(column)))
    }

    fn comparator_powered(&self) -> bool {
        redstone::diode_powered(self.block_state_id(3, 1, ROW_Z))
    }
}

impl ChunkSource for Rig {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        if (cx, cz) == (0, 0) { self.0.lock().unwrap().clone() } else { ChunkColumn::new(MIN_Y, HEIGHT) }
    }
    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        if (x.div_euclid(16), z.div_euclid(16)) == (0, 0) {
            self.0.lock().unwrap().block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
        } else {
            air_state()
        }
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }
    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
        if (x.div_euclid(16), z.div_euclid(16)) == (0, 0) {
            self.0.lock().unwrap().set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), state);
        }
    }
}

struct Loop {
    mobs: MobHandle,
    feed: BlockTickFeed,
    block_entities: BlockEntityHandle,
    rig: Arc<Rig>,
}

impl Loop {
    async fn start(rig: Arc<Rig>, mobs: MobHandle) -> Self {
        let feed = BlockTickFeed::default();
        let block_entities = BlockEntityHandle::default();
        tokio::spawn(run_tick_loop(
            mobs.clone(),
            LiveMobSource::default(),
            block_entities.clone(),
            Arc::new(TickClock::new()),
            Arc::clone(&rig),
            feed.clone(),
            (0..=0, 0..=0),
            ExplosionFeed::default(),
            crate::region_source::ScheduledTickHandle::default(),
            crate::tick_area::TickFollow::default(),
        ));
        tokio::task::yield_now().await;
        Self { mobs, feed, block_entities, rig }
    }

    async fn ticks(&self, n: u64) {
        for _ in 0..n {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
        }
    }

    fn output(&self) -> Option<u8> {
        self.block_entities.with(|registry| registry.comparator_output(COMPARATOR))
    }
}

/// The comparator at the end of the row reads the frame through the stone and
/// follows it as the item goes in, turns through every step, and comes out. The
/// outputs are the reference's `rotation % 8 + 1`.
#[tokio::test(start_paused = true)]
async fn a_comparator_next_to_a_frame_follows_the_rotation() {
    let rig = Rig::new();
    let mobs = MobHandle::new(ChunkWorld::new(MIN_Y, HEIGHT));
    let id = mobs.with(|sim| sim.spawn_item_frame(BlockPos::new(5, 1, ROW_Z), BlockFace::East, false));
    let lp = Loop::start(rig, mobs).await;
    lp.ticks(6).await;
    assert_eq!(lp.output().unwrap_or(0), 0, "an empty frame outputs 0");
    assert!(!lp.rig.comparator_powered());

    lp.mobs.with(|sim| sim.interact_frame(id, Some(&stack("minecraft:diamond")), &never));
    lp.ticks(6).await;
    assert_eq!(lp.output(), Some(1), "an item at rotation 0 outputs 1");
    assert!(lp.rig.comparator_powered());

    for expected in [2u8, 3, 4, 5, 6, 7, 8, 1] {
        lp.mobs.with(|sim| sim.interact_frame(id, None, &never));
        lp.ticks(6).await;
        assert_eq!(lp.output(), Some(expected), "after turning the item");
    }

    lp.mobs.with(|sim| sim.hurt_frame(id, false));
    lp.ticks(6).await;
    assert_eq!(lp.output(), Some(0), "the item is out of the frame");
    assert!(!lp.rig.comparator_powered());
}

/// A blast queued in the shared queue reaches a frame and the loop publishes the
/// break sound that goes with it.
#[tokio::test(start_paused = true)]
async fn a_queued_blast_breaks_a_frame_and_the_loop_sounds_it() {
    let rig = Rig::new();
    let mobs = MobHandle::new(ChunkWorld::new(MIN_Y, HEIGHT));
    mobs.with(|sim| full_frame_at(sim, BlockPos::new(5, 1, ROW_Z)));
    let lp = Loop::start(rig, mobs).await;
    lp.mobs.with(|sim| sim.queue_blast(Vec3::new(5.0, 1.5, 12.0), 3.0, false, false));
    lp.ticks(4).await;
    assert_eq!(lp.mobs.with(|sim| sim.frame_count()), 0);
    let sounds: Vec<String> = lp
        .feed
        .drain_effects_for(uuid::Uuid::nil())
        .into_iter()
        .filter_map(|e| match e {
            crate::effects::WorldEffect::Sound { sound, .. } => Some(sound),
            _ => None,
        })
        .filter(|s| s.contains("item_frame"))
        .collect();
    assert_eq!(sounds, vec!["minecraft:entity.item_frame.break".to_owned()]);
}

/// A frame is not ticked as a burning entity, so fire in its own cell leaves it
/// and its item alone.
#[tokio::test(start_paused = true)]
async fn the_loop_leaves_a_frame_standing_in_fire_alone() {
    let rig = Rig::new();
    rig.set_block(5, 1, ROW_Z, Block::Fire.default_state());
    let mobs = MobHandle::new(ChunkWorld::new(MIN_Y, HEIGHT));
    mobs.with(|sim| full_frame_at(sim, BlockPos::new(5, 1, ROW_Z)));
    let lp = Loop::start(rig, mobs).await;
    lp.ticks(4).await;
    assert_eq!(lp.mobs.with(|sim| sim.frame_count()), 1);
    assert!(lp.mobs.with(|sim| sim.frame_state(only_frame(sim)).and_then(|f| f.2)).is_some(), "the item stays");
}

fn full_frame_at(sim: &mut MobSim<'_>, cell: BlockPos) {
    let id = sim.spawn_item_frame(cell, BlockFace::East, false);
    sim.interact_frame(id, Some(&stack("minecraft:diamond")), &never);
}
