//! Per-block-state physical facts, resolved **once per session** into a flat
//! table indexed by state id (`docs/baritone-port.md` §4.2).
//!
//! The point of the table is that it removes the version call from the inner
//! loop entirely: 32,366 entries of a few words, built once, then every legality
//! and cost question is an array index.
//!
//! # Where the numbers come from
//!
//! Nothing here is transcribed. Geometry is
//! [`VersionAdapter::block_collision`] — the census dumped from the real 26.2
//! server's own block-state registry. The six name-keyed movement constants
//! are [`lodestone_model::block_physics`], anchored to a JVM dump of all 1,196
//! registered blocks. `blocks_motion` is
//! [`VersionAdapter::block_blocks_motion`], the per-state legacy-solid census.
//! See `docs/collision-shapes.md` and `docs/block-physics-constants.md`.
//!
//! The one list that is *ours* is [`MUST_NOT_ENTER`], and it is a policy
//! decision rather than a datum — but the **names** in it are checked against
//! the census by `tests/census.rs`, so a typo cannot silently disarm a hazard.

use lodestone_model::{BlockAabb, DEFAULT_BLOCK_PHYSICS, VersionAdapter, block_physics};

/// The narrow slice of a version's block census this crate needs.
///
/// A trait of its own rather than `&dyn VersionAdapter` directly, for two
/// reasons: a test can implement three methods instead of the adapter's full
/// login/packet surface, and it states exactly which three questions the
/// navigator asks of a version — which is the audit this crate's version-free
/// contract rests on.
pub trait BlockCensus {
    /// Block-local collision boxes for a state. `Some(&[])` is a real answer
    /// (air, water, kelp, cobweb); `None` means "no census for this id".
    fn collision(&self, state: u32) -> Option<&'static [BlockAabb]>;

    /// The canonical `minecraft:*` name of a state, for the name-keyed constants.
    fn name(&self, state: u32) -> Option<&'static str>;

    /// Vanilla's own motion-blocking check, or `None` when there is no census.
    fn blocks_motion(&self, state: u32) -> Option<bool>;
}

/// [`BlockCensus`] over a real [`VersionAdapter`].
///
/// A newtype rather than a blanket impl so a test census can coexist with it
/// under coherence.
#[derive(Debug)]
pub struct AdapterCensus<'a>(pub &'a dyn VersionAdapter);

impl BlockCensus for AdapterCensus<'_> {
    fn collision(&self, state: u32) -> Option<&'static [BlockAabb]> {
        self.0.block_collision(state)
    }

    fn name(&self, state: u32) -> Option<&'static str> {
        self.0.block_name(state)
    }

    fn blocks_motion(&self, state: u32) -> Option<bool> {
        self.0.block_blocks_motion(state)
    }
}

/// Blocks a navigator must never walk, fall or slide into, with the deterrent
/// cost of being *adjacent* to one, in whole ticks.
///
/// `docs/baritone-port.md` §2.3 makes the "must not walk into" family a
/// first-class concept because the sprint-through-a-descent overshoot lands you
/// one cell further along than the plan says, and §4.4's `danger_cost` table
/// gives the relative ordering. These are deterrents, **not durations** — the
/// ordering is what matters, cross-checked against vanilla's own mob-node malus
/// ordering (water 8, breach 4, fire 16, negative = impassable).
///
/// Water is on the list for **M1 only**: swimming is M7, and until then entering
/// water is a plan the executor cannot finish. Remove the row, do not special-case
/// it elsewhere.
pub const MUST_NOT_ENTER: &[(&str, u32)] = &[
    ("minecraft:lava", 200),
    ("minecraft:fire", 400),
    ("minecraft:soul_fire", 400),
    ("minecraft:campfire", 400),
    ("minecraft:soul_campfire", 400),
    ("minecraft:cactus", 60),
    ("minecraft:magma_block", 80),
    ("minecraft:powder_snow", 40),
    ("minecraft:sweet_berry_bush", 20),
    ("minecraft:cobweb", 20),
    ("minecraft:wither_rose", 20),
    ("minecraft:bubble_column", 40),
    ("minecraft:end_portal", 400),
    ("minecraft:nether_portal", 400),
    ("minecraft:end_gateway", 400),
    // M1: no swimming, so water is a wall rather than a route (§4.4, M7).
    ("minecraft:water", 8),
];

/// Everything the graph, the cost model and the physics adapter need about one
/// block state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlockFacts {
    /// Block-local collision boxes — the census slice itself, never a copy.
    pub shape: &'static [BlockAabb],
    /// The shape's own maximum Y extent, block-local and **uncapped**: a fence is `1.5`, a
    /// bottom slab `0.5`, soul sand `0.875`, air `0.0`.
    ///
    /// Clamping this to `1.0` makes a fence look step-able and routes navigation
    /// straight through pens. `lodestone-physics` and the mob pathfinder each
    /// document the same trap independently, which is how you know it has
    /// already been nearly-gotten-wrong twice.
    pub top: f32,
    /// Whether the shape is exactly one full unit cube.
    pub full_cube: bool,
    /// Whether the state has no collision at all. **Not** the same as safe:
    /// cobweb, fire and sweet berry bush are all passable.
    pub passable: bool,
    /// Vanilla's own motion-blocking check.
    pub blocks_motion: bool,
    /// Vanilla's own friction accessor — 0.6 for all but five blocks in 26.2.
    pub friction: f32,
    /// Vanilla's own speed-factor accessor — soul sand and honey are 0.4.
    pub speed_factor: f32,
    /// Vanilla's own jump-factor accessor — honey is 0.5.
    pub jump_factor: f32,
    /// Membership of `#minecraft:climbable`.
    pub climbable: bool,
    /// Whether the cell carries water.
    pub water: bool,
    /// Whether the cell carries lava.
    pub lava: bool,
    /// Vanilla's own stuck-in-block per-axis multiplier, for the three blocks
    /// that grab you.
    pub stuck_multiplier: Option<[f64; 3]>,
    /// Vanilla's own bounce-restitution accessor, already net of the
    /// suppresses-bounce flag.
    pub bounce_restitution: f32,
    /// Whether the navigator refuses to put the body in this cell at all.
    pub must_not_enter: bool,
    /// Deterrent for being adjacent to this cell, in whole ticks.
    pub danger_cost: u32,
}

impl BlockFacts {
    /// The facts of air: no shape, no hazard, vanilla's default constants.
    ///
    /// Used for a cell **outside the snapshot** on the `CollisionView` path only.
    /// On the [`crate::NavView`] path, outside-the-snapshot is `None` and
    /// therefore *illegal* — conflating the two is how a search invents terrain
    /// and produces a beautiful path into unloaded chunks
    /// (`docs/baritone-port.md` §10 trap 3).
    pub const AIR: Self = Self {
        shape: &[],
        top: 0.0,
        full_cube: false,
        passable: true,
        blocks_motion: false,
        friction: DEFAULT_BLOCK_PHYSICS.friction,
        speed_factor: DEFAULT_BLOCK_PHYSICS.speed_factor,
        jump_factor: DEFAULT_BLOCK_PHYSICS.jump_factor,
        climbable: false,
        water: false,
        lava: false,
        stuck_multiplier: None,
        bounce_restitution: DEFAULT_BLOCK_PHYSICS.bounce_restitution,
        must_not_enter: false,
        danger_cost: 0,
    };

    /// What a state id **outside the census** resolves to: an opaque full cube
    /// that must never be entered.
    ///
    /// Pessimistic on purpose. The alternative — treating an unknown id as air —
    /// makes the bot walk confidently into whatever the census does not cover,
    /// and the symptom is a rubber-band, not an error.
    pub const UNKNOWN: Self = Self {
        shape: FULL_CUBE,
        top: 1.0,
        full_cube: true,
        passable: false,
        blocks_motion: true,
        must_not_enter: true,
        ..Self::AIR
    };
}

/// The shape [`BlockFacts::UNKNOWN`] presents.
const FULL_CUBE: &[BlockAabb] = &[BlockAabb {
    min: [0.0, 0.0, 0.0],
    max: [1.0, 1.0, 1.0],
}];

/// How many consecutive state ids with neither a shape nor a name end the scan
/// in [`FactsTable::build`].
const CENSUS_GAP_RUN: u32 = 256;

/// Hard ceiling on the state-id scan, so a misbehaving census cannot make
/// session start unbounded. 26.2 has 32,366 states.
const CENSUS_CEILING: u32 = 1 << 20;

/// Facts for every state id the census covers, indexed by id.
#[derive(Debug, Clone)]
pub struct FactsTable {
    facts: Vec<BlockFacts>,
    /// How many of `facts` came from a real census answer rather than
    /// [`BlockFacts::UNKNOWN`]. Reported so "the table is populated" is a
    /// measurement rather than an assumption.
    resolved: usize,
}

impl FactsTable {
    /// Resolve every state id the census answers for.
    ///
    /// The census exposes no length, so the scan runs until [`CENSUS_GAP_RUN`]
    /// consecutive ids answer neither `collision` nor `name` — which for v26-2
    /// terminates at 32,366 — and is capped at [`CENSUS_CEILING`] regardless.
    #[must_use]
    pub fn build(census: &dyn BlockCensus) -> Self {
        let mut facts: Vec<BlockFacts> = Vec::new();
        let mut resolved = 0usize;
        let mut gap = 0u32;
        let mut last_real = 0usize;

        for state in 0..CENSUS_CEILING {
            let shape = census.collision(state);
            let name = census.name(state);
            if shape.is_none() && name.is_none() {
                gap += 1;
                facts.push(BlockFacts::UNKNOWN);
                if gap >= CENSUS_GAP_RUN {
                    break;
                }
                continue;
            }
            gap = 0;
            resolved += 1;
            last_real = facts.len();
            facts.push(resolve_one(census, state, shape, name));
        }

        facts.truncate(last_real + 1);
        Self { facts, resolved }
    }

    /// An empty table — every state resolves to [`BlockFacts::UNKNOWN`].
    ///
    /// This is what a session with no version family compiled in gets, and it
    /// makes the navigator **refuse to move** rather than plan through terrain
    /// it cannot see. `--features live` is silent when missing (`CLAUDE.md`), so
    /// the failure has to be loud somewhere.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            facts: Vec::new(),
            resolved: 0,
        }
    }

    /// Facts for `state`, or [`BlockFacts::UNKNOWN`] outside the census.
    #[must_use]
    pub fn get(&self, state: u32) -> &BlockFacts {
        self.facts
            .get(state as usize)
            .unwrap_or(&BlockFacts::UNKNOWN)
    }

    /// Number of state ids the table covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.facts.len()
    }

    /// Whether the table covers nothing at all — i.e. the navigator has no world
    /// knowledge and must refuse to plan.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.resolved == 0
    }

    /// How many state ids came from a real census answer.
    #[must_use]
    pub fn resolved(&self) -> usize {
        self.resolved
    }

}

/// One state's facts from the census's three answers plus the name-keyed table.
fn resolve_one(
    census: &dyn BlockCensus,
    state: u32,
    shape: Option<&'static [BlockAabb]>,
    name: Option<&'static str>,
) -> BlockFacts {
    let shape = shape.unwrap_or(&[]);
    let physics = name.map_or(DEFAULT_BLOCK_PHYSICS, block_physics);
    let top = shape.iter().map(|b| b.max[1]).fold(0.0_f32, f32::max);
    let (must_not_enter, danger_cost) = match name.and_then(danger_of) {
        Some(cost) => (true, cost),
        None => (false, 0),
    };

    BlockFacts {
        shape,
        top,
        full_cube: is_full_cube(shape),
        passable: shape.is_empty(),
        // No census answer means the derivation `blocks_motion_at` falls back to
        // is unavailable here too, so say "not solid" and let the shape decide
        // legality. A wrong `blocks_motion` costs a fluid-flow nudge; a wrong
        // shape costs a rubber-band, and the shape is the one we have.
        blocks_motion: census.blocks_motion(state).unwrap_or(false),
        friction: physics.friction,
        speed_factor: physics.speed_factor,
        jump_factor: physics.jump_factor,
        climbable: physics.climbable,
        water: name == Some("minecraft:water"),
        lava: name == Some("minecraft:lava"),
        stuck_multiplier: physics.stuck_multiplier,
        bounce_restitution: physics.bounce_restitution,
        must_not_enter,
        danger_cost,
    }
}

/// The [`MUST_NOT_ENTER`] cost for a block name.
fn danger_of(name: &str) -> Option<u32> {
    MUST_NOT_ENTER
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, cost)| *cost)
}

/// Whether a shape is exactly the unit cube.
fn is_full_cube(shape: &[BlockAabb]) -> bool {
    matches!(shape, [b] if b.min == [0.0, 0.0, 0.0] && b.max == [1.0, 1.0, 1.0])
}

/// A four-state census: 0 air, 1 stone, 2 bottom slab, 3 water.
///
/// **Not `#[cfg(test)]`.** `docs/baritone-port.md` §6 requires a fixture world
/// that structurally contains partial blocks, because both shell collision
/// adapters are coarse in the same way and a rule about slabs can be "verified"
/// against every existing scene and mean nothing. That fixture has to be
/// constructible from an integration test and from the plugin's own gates, so it
/// is public.
#[derive(Debug, Clone, Copy, Default)]
pub struct FixtureCensus;

impl FixtureCensus {
    /// Air.
    pub const AIR: u32 = 0;
    /// A full cube.
    pub const STONE: u32 = 1;
    /// A bottom slab: top `0.5`, under the 0.6 auto-step.
    pub const SLAB: u32 = 2;
    /// Water — collision-free and, in M1, refused.
    pub const WATER: u32 = 3;
    /// Soul sand: top `0.875`, speed factor `0.4`.
    pub const SOUL_SAND: u32 = 4;
    /// Blue ice: friction `0.989`, the slipperiest surface in the game.
    pub const BLUE_ICE: u32 = 5;
    /// An oak fence: top **`1.5`**, which the 0.6 auto-step cannot mount.
    pub const FENCE: u32 = 6;
    /// Lava.
    pub const LAVA: u32 = 7;
    /// A ladder: `#minecraft:climbable`, vanilla's own force-solid-off flag (`blocks_motion` is
    /// `false` despite a nonzero collision shape — see `graph::stand_surface`'s
    /// own doc comment on why that shape must never be read as a stand
    /// surface).
    pub const LADDER: u32 = 8;
}

/// A real ladder's collision shape: vanilla's own per-facing ladder shapes,
/// each a `16.0 x 13.0 x 16.0` box — full `x`/`y`, a thin `z`-slab hugging one
/// face. The exact face is irrelevant to every legality rule in this crate
/// (`graph::stand_surface`'s fix reads only `climbable`, never this shape), so
/// one representative orientation is enough for a fixture.
const FIXTURE_LADDER: &[BlockAabb] = &[BlockAabb {
    min: [0.0, 0.0, 0.8125],
    max: [1.0, 1.0, 1.0],
}];

const FIXTURE_SLAB: &[BlockAabb] = &[BlockAabb {
    min: [0.0, 0.0, 0.0],
    max: [1.0, 0.5, 1.0],
}];
const FIXTURE_SOUL_SAND: &[BlockAabb] = &[BlockAabb {
    min: [0.0, 0.0, 0.0],
    max: [1.0, 0.875, 1.0],
}];
/// A fence's real 26.2 collision shape: a 1.5-tall post plus its connecting arms,
/// reduced to the post because the post is what makes `top == 1.5`.
const FIXTURE_FENCE: &[BlockAabb] = &[BlockAabb {
    min: [0.375, 0.0, 0.375],
    max: [0.625, 1.5, 0.625],
}];

impl BlockCensus for FixtureCensus {
    fn collision(&self, state: u32) -> Option<&'static [BlockAabb]> {
        Some(match state {
            Self::AIR | Self::WATER | Self::LAVA => &[],
            Self::STONE | Self::BLUE_ICE => FULL_CUBE,
            Self::SLAB => FIXTURE_SLAB,
            Self::SOUL_SAND => FIXTURE_SOUL_SAND,
            Self::FENCE => FIXTURE_FENCE,
            Self::LADDER => FIXTURE_LADDER,
            _ => return None,
        })
    }

    fn name(&self, state: u32) -> Option<&'static str> {
        Some(match state {
            Self::AIR => "minecraft:air",
            Self::STONE => "minecraft:stone",
            Self::SLAB => "minecraft:stone_slab",
            Self::WATER => "minecraft:water",
            Self::SOUL_SAND => "minecraft:soul_sand",
            Self::BLUE_ICE => "minecraft:blue_ice",
            Self::FENCE => "minecraft:oak_fence",
            Self::LAVA => "minecraft:lava",
            Self::LADDER => "minecraft:ladder",
            _ => return None,
        })
    }

    fn blocks_motion(&self, state: u32) -> Option<bool> {
        // Vanilla's own solidity override: a ladder never blocks motion despite its nonzero
        // collision shape (`graph::stand_surface`'s doc comment).
        Some(!matches!(
            state,
            Self::AIR | Self::WATER | Self::LAVA | Self::LADDER
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_stops_at_the_end_of_the_census() {
        let table = FactsTable::build(&FixtureCensus);
        assert_eq!(table.resolved(), 9);
        assert_eq!(table.len(), 9);
        assert!(!table.is_empty());
    }

    /// The tag-membership constant reaches the fixture, exactly like the
    /// other name-keyed constants below — this is what makes a `Climb` gate
    /// against `FixtureCensus::LADDER` a real exercise of `climbable` rather
    /// than an assumption that it is wired.
    #[test]
    fn the_ladder_fixture_is_climbable_and_does_not_block_motion() {
        let table = FactsTable::build(&FixtureCensus);
        let facts = table.get(FixtureCensus::LADDER);
        assert!(facts.climbable);
        assert!(
            !facts.blocks_motion,
            "forceSolidOff: a ladder never blocks motion despite a nonzero shape"
        );
        assert!(facts.top > 0.0, "the shape itself is real, just not a support");
    }

    #[test]
    fn tops_are_uncapped_and_local() {
        let table = FactsTable::build(&FixtureCensus);
        assert_eq!(table.get(FixtureCensus::STONE).top, 1.0);
        assert_eq!(table.get(FixtureCensus::SLAB).top, 0.5);
        assert_eq!(table.get(FixtureCensus::SOUL_SAND).top, 0.875);
        assert_eq!(
            table.get(FixtureCensus::FENCE).top,
            1.5,
            "a fence must not be capped to 1.0 — clamping routes navigation \
             through pens"
        );
        assert_eq!(table.get(FixtureCensus::AIR).top, 0.0);
    }

    /// An id past the census is an opaque wall, never air. The reverse is the
    /// failure that produces a confident path into nothing.
    #[test]
    fn an_unknown_state_is_a_wall_not_air() {
        let table = FactsTable::build(&FixtureCensus);
        let facts = table.get(9_999);
        assert!(facts.must_not_enter);
        assert!(!facts.passable);
        assert!(facts.full_cube);
    }

    #[test]
    fn water_and_lava_are_refused_in_m1() {
        let table = FactsTable::build(&FixtureCensus);
        for state in [FixtureCensus::WATER, FixtureCensus::LAVA] {
            let facts = table.get(state);
            assert!(facts.must_not_enter);
            assert!(facts.passable, "…but it still has no collision box");
        }
        assert!(table.get(FixtureCensus::WATER).water);
        assert!(table.get(FixtureCensus::LAVA).lava);
    }

    #[test]
    fn an_empty_table_reports_itself_empty_so_the_navigator_can_refuse() {
        assert!(FactsTable::empty().is_empty());
        assert!(FactsTable::empty().get(0).must_not_enter);
    }

    /// The name-keyed constants really are reached, not defaulted — which is the
    /// difference between a fixture that exercises `SurfaceClass` and one that
    /// silently reports 0.6 everywhere.
    #[test]
    fn name_keyed_constants_reach_the_table() {
        let table = FactsTable::build(&FixtureCensus);
        assert_eq!(table.get(FixtureCensus::BLUE_ICE).friction, 0.989);
        assert_eq!(table.get(FixtureCensus::SOUL_SAND).speed_factor, 0.4);
        assert_eq!(table.get(FixtureCensus::STONE).friction, 0.6);
    }

    /// Every hazard name must be spellable. This test cannot see the real census
    /// (that gate lives in `tests/census.rs`, which needs a version crate), so it
    /// checks the weaker but still useful property that the list has no
    /// duplicates — a duplicate silently shadows a different cost.
    #[test]
    fn the_hazard_list_is_well_formed() {
        let mut names: Vec<&str> = MUST_NOT_ENTER.iter().map(|(n, _)| *n).collect();
        names.sort_unstable();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "duplicate hazard name");
        assert!(
            MUST_NOT_ENTER
                .iter()
                .all(|(n, c)| n.starts_with("minecraft:") && *c > 0)
        );
    }
}
