//! Can a goal read a block — and does grass actually become dirt?
//!
//! # What it is
//!
//! The behavioural gate for issue #456. `MobController` declared 33 methods and
//! **not one read a block**, so every vanilla goal whose predicate consults the
//! world was inexpressible; a sheep that eats grass could not ask whether there
//! was grass. This drives the seam that fixed it end to end: a real
//! [`NavigatingMob`] over a real [`PathWorld`], `EatBlockGoal` installed only by
//! [`goals_for`], and a world that *applies* the drained eat intent, so the
//! assertion is on the block, not on a counter.
//!
//! # The honest limit on "end to end"
//!
//! [`GrassWorld`] plays the host's part — it drains
//! [`NavigatingMob::take_new_eaten`] and performs the mutation
//! [`EatenBlock`] describes. In production that drain belongs to
//! `lodestone_server::mobs::MobSim::tick`, which is another agent's file, so it
//! arrives as a brokered patch. **Until that patch lands, production grazing
//! stops one hop short**: the goal fires, the animation plays, the intent is
//! recorded and nothing consumes it. This file cannot close that gap and does
//! not pretend to — it proves the seam and the goal, and names what is missing.
//!
//! # Why not `ScriptMob` or `ai/roster/probe.rs`
//!
//! Both override perception wholesale, which is how #441's island and #455's
//! stayed hidden. A block-perception gate written against a double that answers
//! `block_cues_*` from a field would pass with `NavigatingMob`'s override
//! missing entirely.
//!
//! # The goal now comes from the roster, and this file no longer stubs it
//!
//! [`graze`] used to install the roster's whole set **and then add
//! `EatBlockGoal` at its jar priority of 5**, because the sheep's row was
//! `Coverage::Missing` in `ai/roster/passive.rs` and flipping it was a brokered
//! patch. That row now carries the goal, so both stub `add` calls are gone and
//! every goal these gates observe arrives through [`goals_for`].
//!
//! **Leaving the stub in place would have been a silent double-install, not a
//! harmless duplicate.** Two `EatBlockGoal`s at priority 5 each draw their own
//! `next_i32(interval)`, so grazing happens at roughly twice the rate:
//! `the_grazing_interval_is_the_halved_delay_and_not_the_jar_literal` measured
//! **627** eats against its predicted 444 the moment the row flipped, which is
//! how the duplicate was caught rather than shipped.
//!
//! What these gates still cannot tell you is whether a *running game's* sheep
//! grazes. That needs the host half — `ChunkWorld::block_cues` to classify the
//! block, and the drained eat applied where mutable chunk access lives — in
//! `lodestone-server`, owned elsewhere.

use std::collections::HashMap;
use std::sync::Mutex;

use lodestone_entity::ai::goals::EatBlockGoal;
use lodestone_entity::ai::mob::EatenBlock;
use lodestone_entity::ai::navigating_mob::BABY_START_AGE;
use lodestone_entity::ai::{GoalSelector, NavigatingMob, SpeciesContext, goals_for};
use lodestone_entity::pathfinding::{Aabb, BlockCues, MobShape, PathType, PathWorld};
use lodestone_model::Vec3;

/// The blocks this gate needs to tell apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Block {
    Air,
    /// `minecraft:grass_block` — grazeable from above, becomes [`Block::Dirt`].
    Grass,
    /// `minecraft:dirt` — what grazed grass becomes, and *not* itself grazeable.
    /// The distinction is the whole point: a cue table that answered "solid" for
    /// both could not express the mutation.
    Dirt,
    /// `minecraft:stone` — the negative control's floor.
    Stone,
    /// A `#minecraft:edible_for_sheep` block occupying the mob's own position,
    /// e.g. `short_grass`. Destroyed outright rather than replaced.
    ShortGrass,
}

impl Block {
    fn cues(self) -> BlockCues {
        BlockCues {
            edible_for_sheep: matches!(self, Block::ShortGrass),
            grass_block: matches!(self, Block::Grass),
        }
    }

    fn solid(self) -> bool {
        matches!(self, Block::Grass | Block::Dirt | Block::Stone)
    }
}

/// A flat world of one block type, standing in for the host: it classifies
/// blocks for the seam **and** applies the mutations a drained
/// [`EatenBlock`] asks for.
///
/// `Mutex` rather than `RefCell` because [`PathWorld`] is `Send + Sync` (the
/// integrated server hands the sim to a spawned task). The mob borrows this
/// immutably for its whole life, so the eat mutation has to go through interior
/// mutability, exactly as a real chunk store's would.
struct GrassWorld {
    /// Everything at `y == -1`; `y <= -2` is always stone.
    floor: Mutex<HashMap<(i32, i32), Block>>,
    /// Blocks at `y == 0`, the layer the mob stands *in*.
    at_feet: Mutex<HashMap<(i32, i32), Block>>,
    /// The default for any floor column not named in `floor`.
    default_floor: Block,
    /// The default for any `y == 0` column not named in `at_feet`.
    default_at_feet: Block,
}

impl GrassWorld {
    fn new(default_floor: Block) -> Self {
        Self {
            floor: Mutex::new(HashMap::new()),
            at_feet: Mutex::new(HashMap::new()),
            default_floor,
            default_at_feet: Block::Air,
        }
    }

    /// Fills the whole `y == 0` layer with an edible block, so the mob is
    /// standing *in* grass wherever it wanders.
    fn with_short_grass_everywhere(mut self) -> Self {
        self.default_at_feet = Block::ShortGrass;
        self
    }

    fn block(&self, x: i32, y: i32, z: i32) -> Block {
        match y {
            0 => *self
                .at_feet
                .lock()
                .unwrap()
                .get(&(x, z))
                .unwrap_or(&self.default_at_feet),
            -1 => *self
                .floor
                .lock()
                .unwrap()
                .get(&(x, z))
                .unwrap_or(&self.default_floor),
            y if y <= -2 => Block::Stone,
            _ => Block::Air,
        }
    }

    /// What the brokered `MobSim::tick` drain owes each eaten block
    /// (`ai/goal/EatBlockGoal.java:59-80`). `mobGriefing` is assumed on, which is
    /// vanilla's default.
    fn apply(&self, what: EatenBlock, x: i32, z: i32) {
        match what {
            EatenBlock::AtFeet => {
                self.at_feet.lock().unwrap().insert((x, z), Block::Air);
            }
            EatenBlock::Below => {
                self.floor.lock().unwrap().insert((x, z), Block::Dirt);
            }
        }
    }

    /// How many floor columns are dirt — the observable this gate is really
    /// about.
    fn dirt_columns(&self) -> usize {
        self.floor
            .lock()
            .unwrap()
            .values()
            .filter(|b| **b == Block::Dirt)
            .count()
    }
}

impl PathWorld for GrassWorld {
    fn min_y(&self) -> i32 {
        -8
    }

    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if self.block(x, y, z).solid() {
            PathType::Blocked
        } else {
            PathType::Open
        }
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        if self.block(x, y, z).solid() { 1.0 } else { 0.0 }
    }

    fn collides(&self, aabb: Aabb) -> bool {
        let (x0, x1) = (aabb.min_x.floor() as i32, (aabb.max_x - 1e-7).floor() as i32);
        let (y0, y1) = (aabb.min_y.floor() as i32, (aabb.max_y - 1e-7).floor() as i32);
        let (z0, z1) = (aabb.min_z.floor() as i32, (aabb.max_z - 1e-7).floor() as i32);
        (x0..=x1).any(|x| (y0..=y1).any(|y| (z0..=z1).any(|z| self.block(x, y, z).solid())))
    }

    fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }

    fn block_cues(&self, x: i32, y: i32, z: i32) -> BlockCues {
        self.block(x, y, z).cues()
    }
}

/// What one grazing run observed.
struct Graze {
    /// Every eaten block, with the tick it happened on.
    eaten: Vec<(usize, EatenBlock)>,
    /// Floor columns that ended up dirt.
    dirt: usize,
    /// The largest distance the mob moved on any single tick during the 18 ticks
    /// leading up to its first eat — vanilla stops the navigation for the whole
    /// animation (`ai/goal/EatBlockGoal.java:41`).
    max_step_while_eating: f64,
}

/// Spawns a real sheep on `world`, installs whatever the roster gives it, and
/// ticks — draining the eat intent into the world each tick the way the host
/// must.
///
/// `deplete` decides whether the drained intent is actually applied to the
/// world. It matters more than it looks: with mutations on, a grazing sheep
/// turns its own column to dirt and then cannot graze again until it wanders to
/// fresh grass, so the observed *rate* is bounded by how fast a 0.23 blocks/tick
/// animal finds new grass rather than by the goal's interval. That is correct
/// behaviour and it is why the interval gate below runs with `deplete: false` —
/// measuring an interval against a depleting world measures the depletion.
fn graze(world: &GrassWorld, baby: bool, ticks: usize, deplete: bool) -> Graze {
    let step = 0.23;
    let ctx = SpeciesContext::new(step);
    let mut mob = NavigatingMob::new(
        world,
        MobShape::land(0.9, 1.3),
        Vec3::new(0.5, 0.0, 0.5),
        step,
        256,
        0x1234_5678_9ABC_DEF0,
    );
    if baby {
        mob.set_age(BABY_START_AGE);
    }

    let mut ai = GoalSelector::new();
    for (priority, goal) in goals_for("sheep", &ctx) {
        ai.add(priority, goal);
    }

    let mut eaten = Vec::new();
    let mut positions: Vec<Vec3> = Vec::new();
    let mut max_step_while_eating = 0.0_f64;
    for t in 0..ticks {
        positions.push(mob.position());
        mob.tick(&mut ai);
        let p = mob.block_position();
        for what in mob.take_new_eaten() {
            if eaten.is_empty() {
                // The animation runs for EAT_ANIMATION_TICKS and consumes at
                // CONSUME_AT, so the mob has been standing still for the
                // difference. Measure it from the recorded positions.
                let span = (EatBlockGoal::EAT_ANIMATION_TICKS - EatBlockGoal::CONSUME_AT) as usize;
                let first = t.saturating_sub(span);
                for w in positions[first..=t.min(positions.len() - 1)].windows(2) {
                    max_step_while_eating = max_step_while_eating.max((w[1] - w[0]).length());
                }
            }
            eaten.push((t, what));
            if deplete {
                world.apply(what, p.x, p.z);
            }
        }
    }

    Graze {
        eaten,
        dirt: world.dirt_columns(),
        max_step_while_eating,
    }
}

/// The headline, both directions: a sheep standing on grass grazes and **the
/// block becomes dirt**; a sheep standing on stone, ticked exactly as long,
/// never eats at all.
///
/// The stone arm is the control that makes the grass arm mean something. Without
/// it, a goal whose `can_use` ignored the cues entirely and always fired would
/// pass the grass arm perfectly.
#[test]
fn a_sheep_on_grass_eats_and_the_block_becomes_dirt_a_sheep_on_stone_does_not() {
    let ticks = 4000;

    let grass = GrassWorld::new(Block::Grass);
    let on_grass = graze(&grass, false, ticks, true);
    assert!(
        !on_grass.eaten.is_empty(),
        "a sheep stood on grass for {ticks} ticks and never grazed. Either the \
         roster does not install EatBlockGoal, or block_cues_below is not \
         reaching the world"
    );
    assert!(
        on_grass.dirt >= 1,
        "the sheep ate {} times but no floor column is dirt — the EatenBlock \
         intent is not describing the mutation vanilla performs",
        on_grass.eaten.len()
    );
    assert!(
        on_grass
            .eaten
            .iter()
            .all(|(_, w)| *w == EatenBlock::Below),
        "a sheep standing on a grass block with air at its feet must report \
         EatenBlock::Below, since there is nothing edible to destroy in place"
    );

    let stone = GrassWorld::new(Block::Stone);
    let on_stone = graze(&stone, false, ticks, true);
    assert!(
        on_stone.eaten.is_empty(),
        "a sheep on stone grazed {} times. EatBlockGoal's predicate is not \
         reading the block, so the grass arm above proves nothing",
        on_stone.eaten.len()
    );
    assert_eq!(
        on_stone.dirt, 0,
        "stone became dirt, so this world's mutation has a path that is not the \
         eat drain — every assertion here would be measuring that instead"
    );
}

/// The other branch of vanilla's predicate: an *edible* block at the mob's own
/// position is destroyed in place rather than turning the floor to dirt
/// (`ai/goal/EatBlockGoal.java:63-68` vs `:71-78`), and it takes priority over
/// the block below.
#[test]
fn an_edible_block_at_the_mobs_feet_is_eaten_in_place_and_wins_over_the_floor() {
    // Grass floor *and* short grass at the mob's own column, so both branches
    // are satisfiable and only the order decides.
    let world = GrassWorld::new(Block::Grass).with_short_grass_everywhere();
    let r = graze(&world, false, 4000, true);

    let first = r.eaten.first().map(|(_, w)| *w);
    assert_eq!(
        first,
        Some(EatenBlock::AtFeet),
        "with both an edible block at its feet and grass below, vanilla checks \
         the feet first; got {first:?}"
    );
}

/// The sheep stands still while it eats. Vanilla stops the navigation in
/// `start` (`ai/goal/EatBlockGoal.java:41`) and the goal claims MOVE, LOOK and
/// JUMP (`:24`) so `WaterAvoidingRandomStrollGoal` at the next priority down
/// cannot preempt it.
///
/// This is the assertion that would fail if the goal were given the wrong flag
/// set — a mistake no multiset or coverage gate can see, because the row would
/// still be present at the right priority.
#[test]
fn a_grazing_sheep_holds_still_for_the_whole_animation() {
    let world = GrassWorld::new(Block::Grass);
    let r = graze(&world, false, 4000, true);
    assert!(!r.eaten.is_empty(), "precondition: the sheep must graze");
    assert_eq!(
        r.max_step_while_eating, 0.0,
        "the sheep moved while grazing; EatBlockGoal must stop the navigation \
         and hold MOVE for the whole animation"
    );
}

/// A baby grazes far more often than an adult — and the *ratio* pins the
/// halving that `Goal.adjustedTickDelay` applies.
///
/// Vanilla's literals are `1000` and `50`, but neither is the number of ticks
/// that elapses: `adjustedTickDelay` is `positiveCeilDiv(t, 2)` for a goal that
/// does not override `requiresUpdateEveryTick`, and `EatBlockGoal` does not
/// (`ai/goal/Goal.java:53-55`). So the real intervals are **500 and 25**.
///
/// Both hypotheses are computed from outside constants and the measurement must
/// land on one. A grazing cycle costs `interval + EAT_ANIMATION_TICKS` ticks on
/// average, so over `ticks`:
///
/// | hypothesis | baby interval | expected eats in 20 000 ticks |
/// |---|---|---|
/// | halved (correct) | 25 | ≈ 444 |
/// | jar literal, unhalved | 50 | ≈ 285 |
///
/// A ±25% band around the correct figure excludes the wrong one, which a test
/// asserting only "the baby ate more often" could never do.
#[test]
fn the_grazing_interval_is_the_halved_delay_and_not_the_jar_literal() {
    let ticks = 20_000;

    let world = GrassWorld::new(Block::Grass);
    let baby = graze(&world, true, ticks, false);

    let cycle = |interval: i32| (interval + EatBlockGoal::EAT_ANIMATION_TICKS) as f64;
    let predicted = ticks as f64 / cycle(EatBlockGoal::BABY_INTERVAL);
    let unhalved = ticks as f64 / cycle(EatBlockGoal::BABY_INTERVAL * 2);
    let got = baby.eaten.len() as f64;

    assert!(
        (got - predicted).abs() < 0.25 * predicted,
        "a baby sheep grazed {got} times in {ticks} ticks; the halved interval \
         ({} ticks) predicts {predicted:.0} and the unhalved jar literal ({}) \
         predicts {unhalved:.0}",
        EatBlockGoal::BABY_INTERVAL,
        EatBlockGoal::BABY_INTERVAL * 2
    );
    assert!(
        (got - unhalved).abs() > 0.25 * predicted,
        "{got} eats cannot distinguish the halved interval from the unhalved \
         one ({predicted:.0} vs {unhalved:.0}); this gate is vacuous as written"
    );

    // And the adult arm, for the same reason in the other direction: the same
    // world, the same seed, 20× the interval.
    let adult_world = GrassWorld::new(Block::Grass);
    let adult = graze(&adult_world, false, ticks, false);
    let adult_predicted = ticks as f64 / cycle(EatBlockGoal::ADULT_INTERVAL);
    assert!(
        (adult.eaten.len() as f64 - adult_predicted).abs() < 0.5 * adult_predicted,
        "an adult sheep grazed {} times where {adult_predicted:.0} was predicted \
         from a {}-tick interval",
        adult.eaten.len(),
        EatBlockGoal::ADULT_INTERVAL
    );
}

/// The seam's default is inert, and that is worth an assertion rather than a
/// comment: a host whose `PathWorld` does not classify blocks makes every
/// cue-reading goal silently do nothing.
///
/// This is the shape of the next island in this area, so it is pinned here — if
/// someone later changes `BlockCues`' default to something permissive, a sheep
/// would graze bare stone and this fails.
#[test]
fn a_world_that_classifies_no_blocks_leaves_grazing_inert() {
    struct Blind;
    impl PathWorld for Blind {
        fn min_y(&self) -> i32 {
            -8
        }
        fn base_path_type(&self, _x: i32, y: i32, _z: i32) -> PathType {
            if y <= -1 { PathType::Blocked } else { PathType::Open }
        }
        fn collision_top(&self, _x: i32, y: i32, _z: i32) -> f64 {
            if y <= -1 { 1.0 } else { 0.0 }
        }
        fn collides(&self, aabb: Aabb) -> bool {
            aabb.min_y < 0.0
        }
    }

    let world = Blind;
    let ctx = SpeciesContext::new(0.23);
    let mut mob = NavigatingMob::new(
        &world,
        MobShape::land(0.9, 1.3),
        Vec3::new(0.5, 0.0, 0.5),
        0.23,
        256,
        0,
    );
    let mut ai = GoalSelector::new();
    for (priority, goal) in goals_for("sheep", &ctx) {
        ai.add(priority, goal);
    }
    // A baby, so the interval is short enough that 4000 ticks is many chances.
    mob.set_age(BABY_START_AGE);
    for _ in 0..4000 {
        mob.tick(&mut ai);
    }
    assert!(
        mob.eaten().is_empty(),
        "a world that answers no block cues let a sheep graze {} times — the \
         default must be BlockCues::NONE, or a mob grazes bare stone",
        mob.eaten().len()
    );
}
