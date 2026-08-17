//! The sniffer's own seek/dig/rise/egg-drop state machine (issue #230's last
//! remaining species) — `Sniffer.State`'s `IDLING -> SNIFFING -> SEARCHING ->
//! DIGGING -> RISING` loop, `Sniffer`/`SnifferAi`'s own timers and block
//! search collapsed into one host-side per-mob driver, the same shape
//! `mobs::warden` already established for a Brain-adjacent species this
//! crate has no seam to drive purely through `Brain`/`BrainMob`.
//!
//! # Division of labour with the Brain half
//!
//! `lodestone_entity::brain::roster::sniffer_brain`'s own doc names the split
//! precisely: this module resolves *where* to dig (a block-tag search over
//! real world data, something `BrainMob` cannot read) and *when* each phase
//! ends (the timers below), and hands the Brain only a walk target through
//! `BrainMob::sniffer_dig_target` while [`SnifferState::Searching`]. The
//! Brain's own `WalkToPoi`/`MoveToTargetSink` do the actual pathfinding;
//! [`MobSim::tick_sniffers`] detects arrival by comparing its own position
//! against the target it handed out, not by reading anything back out of
//! that `Brain` — the same one-way constraint `camel_random_sitting`'s own
//! doc already names for a goal-driven signal this seam likewise cannot see.
//!
//! # Disclosed narrowings against `Sniffer`/`SnifferAi`
//!
//! - **`FEELING_HAPPY`/`SCENTING`** — both purely cosmetic animation states
//!   with no gameplay consequence — are not built at all. A finished dig
//!   returns straight to `IDLING` rather than a brief happy dance first, and
//!   a sniffer never scents.
//! - **The "start sniffing" trigger is an independent per-tick coin flip**
//!   ([`sniff_roll`]), not vanilla's weighted `RunOne` pick among six `IDLE`
//!   options — the same disclosed shape `camel_random_sitting`'s own doc
//!   already establishes for a Brain-internal choice this seam cannot
//!   observe.
//! - **The dig-position search is a bounded box scan around the sniffer's
//!   own feet** ([`find_dig_position`]), not `LandRandomPos`'s
//!   pathfinding-aware sampling, and skips the real reachability check
//!   (`Path::canReach`) entirely — the same "no bounding-box on this seam"
//!   cut `RamTarget`'s own doc already discloses for a different species. A
//!   dig position behind unnavigable terrain fails closed through the
//!   Brain's own `WalkToPoi` -> `MoveToTargetSink` (never arrives, so
//!   `Searching` times out) rather than being filtered out up front.
//! - **The stored target is the walkable cell above the diggable block**,
//!   not the diggable block's own position vanilla's `WalkTarget` names —
//!   an adaptation for a "walk toward a raw `Vec3`" system with no
//!   ground-snap of its own, not a behavioural difference.
//! - **Digging particles/sounds and the head-forward drop offset are not
//!   modelled** — the loot drops at the sniffer's own current position, the
//!   same `resolve_cat_gifts`-established simplification for "no
//!   `randomTeleport`/facing-offset seam" this crate already discloses
//!   elsewhere.
//! - **Mid-dig cancellation only checks panic**, not vanilla's fuller
//!   `canStillUse` (`SNIFFER_DIGGING present && canDig() && !isInLove()`).
//!
//! # How to change it
//!
//! - **Timers**: the constants below, each cited from `Sniffer`/`SnifferAi`.
//! - **Dig-search radius/shape**: [`find_dig_position`] — the one function
//!   that reads [`ChunkWorld`].
//! - **Eligibility**: [`eligible_to_sniff`]/[`eligible_to_dig`], mirroring
//!   `Sniffer.canSniff`/`canDig` minus the checks this seam cannot make
//!   (`isTempted`, `onGround`, `isPassenger`).
//!
//! # Dependencies
//!
//! [`crate::block_drops::bundled_tables`] for the real `gameplay/sniffer_digging`
//! loot table (torchflower seeds / pitcher pod); nothing else new.

use lodestone_model::{BlockPos, ResourceKey, Vec3};

use super::villager::bare_block_id;
use super::{ChunkWorld, MobSim, SimMob};
use crate::mob_spawn::SpawnRng;

/// `SnifferAi.Sniffing`'s own duration range.
pub const SNIFFING_MIN_TICKS: i32 = 40;
/// See [`SNIFFING_MIN_TICKS`].
pub const SNIFFING_MAX_TICKS: i32 = 80;

/// `SnifferAi.Searching`'s own single-duration constructor argument — real
/// vanilla's `Searching.canStillUse` actually stops the instant the walk
/// target is reached (an arrival this module detects itself, see the module
/// doc), so this is purely the "gave up" timeout.
pub const SEARCHING_TIMEOUT_TICKS: i32 = 600;

/// `SnifferAi.Digging`'s own duration range.
pub const DIGGING_MIN_TICKS: i32 = 160;
/// See [`DIGGING_MIN_TICKS`].
pub const DIGGING_MAX_TICKS: i32 = 180;

/// `SnifferAi.FinishedDigging`'s own fixed duration (`min == max == 40`).
pub const RISING_TICKS: i32 = 40;

/// `Sniffer.SNIFFING_COOLDOWN_TICKS` — how long after a finished dig before
/// the next sniff may start.
pub const SNIFF_COOLDOWN_TICKS: i32 = 9600;

/// `Sniffer.storeExploredPosition`'s own `limit(20L)`.
pub const EXPLORED_POSITIONS_CAP: usize = 20;

/// Not a vanilla constant — the horizontal half-width of
/// [`find_dig_position`]'s bounded scan, standing in for
/// `LandRandomPos.getPos`'s smallest attempted radius (`10 + 2*0`). See the
/// module doc's disclosed cut on this search.
const DIG_SEARCH_HORIZONTAL_RADIUS: i32 = 10;

/// Not a vanilla constant — the vertical half-height of the same scan,
/// matching `LandRandomPos.getPos`'s own `3`-block argument.
const DIG_SEARCH_VERTICAL_RADIUS: i32 = 3;

/// Not a vanilla constant — how close (squared, blocks) counts as "arrived"
/// at a dig target. [`lodestone_entity::brain::roster::sniffer_brain`]'s own
/// `WalkToPoi` uses vanilla's real `close_enough = 0` for the *walk*, but
/// this host-side arrival check needs its own tolerance since it has no
/// pathfinding-exact-arrival signal to read back — chosen tight enough that
/// "arrived" cannot be true well outside melee-adjacent range.
const DIG_ARRIVAL_DISTANCE_SQR: f64 = 2.25;

/// Not a vanilla constant — how far (squared, blocks) a candidate must be
/// from every position in [`SimMob`]'s own explored list to still count as
/// unexplored. Vanilla compares exact `GlobalPos` equality; this crate's
/// search is a continuous scan rather than vanilla's five discrete
/// candidates, so an exact-equality check would almost never fire and the
/// sniffer would happily re-dig one block over. A small radius is the
/// honest analogue.
const EXPLORED_AVOID_DISTANCE_SQR: f64 = 4.0;

/// `#minecraft:sniffer_diggable_block` (`tags/block/sniffer_diggable_block.json`),
/// fully expanded: `#minecraft:dirt` (dirt/coarse_dirt/rooted_dirt),
/// `#minecraft:mud` (mud/muddy_mangrove_roots), `#minecraft:moss_blocks`
/// (moss_block/pale_moss_block), plus the two direct members
/// (grass_block/podzol).
const DIGGABLE_BLOCKS: &[&str] = &[
    "dirt",
    "coarse_dirt",
    "rooted_dirt",
    "mud",
    "muddy_mangrove_roots",
    "moss_block",
    "pale_moss_block",
    "grass_block",
    "podzol",
];

/// `Sniffer.State`, narrowed to the five states this crate actually
/// produces — see the module doc's disclosed `FEELING_HAPPY`/`SCENTING`
/// omission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SnifferState {
    /// `Sniffer.State.IDLING` — the default resting state.
    #[default]
    Idling,
    /// `Sniffer.State.SNIFFING` — head down, about to pick a dig target.
    Sniffing,
    /// `Sniffer.State.SEARCHING` — walking toward
    /// [`SimMob::sniffer_dig_target`](super::SimMob).
    Searching,
    /// `Sniffer.State.DIGGING` — stationary, counting down to the loot roll.
    Digging,
    /// `Sniffer.State.RISING` — stationary, counting down back to `IDLING`.
    Rising,
}

impl SnifferState {
    /// `Sniffer.State`'s real jar ordinal (`net.minecraft.world.entity.animal.sniffer.Sniffer.State`)
    /// for each state this crate produces. `1`/`2` (`FEELING_HAPPY`/
    /// `SCENTING`) are skipped entirely, not merely unused — see this
    /// module's own doc.
    #[must_use]
    pub fn wire_ordinal(self) -> u8 {
        match self {
            Self::Idling => 0,
            Self::Sniffing => 3,
            Self::Searching => 4,
            Self::Digging => 5,
            Self::Rising => 6,
        }
    }
}

/// `Sniffer.canSniff()`, minus the checks this seam cannot make at all
/// (`isTempted` — no temptation-in-progress read on `SimMob`; `onGround`/
/// `isPassenger` — no ground/mount state tracked for a walking mob).
fn eligible_to_sniff(m: &SimMob<'_>) -> bool {
    !m.is_panicking() && !m.in_water() && !m.is_in_love() && !m.is_leashed()
}

/// `Sniffer.canDig()`, the same narrowing [`eligible_to_sniff`] discloses.
fn eligible_to_dig(m: &SimMob<'_>) -> bool {
    !m.is_panicking() && !m.is_baby() && !m.in_water() && !m.is_leashed()
}

/// An independent, deterministic per-tick coin flip standing in for
/// vanilla's weighted `RunOne` pick — see the module doc's disclosed cut.
/// The same salted-hash shape `camel_sit_roll` already establishes for an
/// identical class of approximation.
fn sniff_roll(id: i32, tick_count: u64) -> bool {
    let mix = tick_count
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(id as u64)
        .wrapping_mul(1_442_695_040_888_963_407)
        >> 33;
    mix % 200 == 0
}

/// A single-draw RNG seeded from this mob's id, the current tick, and a
/// per-call-site salt so two different rolls for the same mob on the same
/// tick (e.g. a sniffing duration and, later that same tick, a digging
/// duration) do not correlate — the same per-event seeding shape
/// `resolve_cat_gifts` already uses.
fn event_rng(id: i32, tick_count: u64, salt: u64) -> SpawnRng {
    SpawnRng::new(
        tick_count
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ (id as u64).wrapping_mul(0xD6E8_FEB8_6659_FD93)
            ^ salt,
    )
}

/// A duration uniformly drawn from `[min, max]` inclusive, matching
/// [`lodestone_entity::brain::behavior::Leaf`]'s own `min + next_i32(max + 1
/// - min)` roll for a timed `Behavior`.
fn roll_duration(rng: &mut SpawnRng, min: i32, max: i32) -> i32 {
    min + rng.next_int(max + 1 - min)
}

/// `Sniffer.calculateDigPosition`, collapsed to a single bounded box scan —
/// see the module doc's disclosed cut against `LandRandomPos`'s
/// pathfinding-aware five-candidate search. Returns the nearest diggable,
/// headroom-clear, not-recently-explored candidate's own **walkable cell**
/// (one block above the diggable block itself — see the module doc for why
/// that is not the position vanilla's `WalkTarget` names).
fn find_dig_position(world: &ChunkWorld, sniffer: &SimMob<'_>) -> Option<Vec3> {
    let pos = sniffer.position();
    let origin = BlockPos::new(pos.x.floor() as i32, pos.y.floor() as i32, pos.z.floor() as i32);
    let mut best: Option<(f64, Vec3)> = None;
    for dx in -DIG_SEARCH_HORIZONTAL_RADIUS..=DIG_SEARCH_HORIZONTAL_RADIUS {
        for dz in -DIG_SEARCH_HORIZONTAL_RADIUS..=DIG_SEARCH_HORIZONTAL_RADIUS {
            for dy in -DIG_SEARCH_VERTICAL_RADIUS..=DIG_SEARCH_VERTICAL_RADIUS {
                let x = origin.x + dx;
                let y = origin.y + dy;
                let z = origin.z + dz;
                let bare = bare_block_id(world.block_state(x, y, z));
                if !DIGGABLE_BLOCKS.contains(&bare) {
                    continue;
                }
                // Headroom for the walkable cell directly above the
                // diggable block — the same `#air`-tag approximation
                // `find_nearest_cat_block`'s own doc already discloses for
                // a different species' block search.
                let above = bare_block_id(world.block_state(x, y + 1, z));
                if !matches!(above, "air" | "cave_air" | "void_air") {
                    continue;
                }
                let candidate = Vec3::new(f64::from(x) + 0.5, f64::from(y + 1), f64::from(z) + 0.5);
                if sniffer
                    .sniffer_explored
                    .iter()
                    .any(|&explored| super::dist_sqr(explored, candidate) < EXPLORED_AVOID_DISTANCE_SQR)
                {
                    continue;
                }
                let d = super::dist_sqr(pos, candidate);
                if best.is_none() || best.is_some_and(|(best_d, _)| d < best_d) {
                    best = Some((d, candidate));
                }
            }
        }
    }
    best.map(|(_, pos)| pos)
}

/// The diggable block directly below `walk_target` (the walkable cell
/// [`find_dig_position`] returned), re-checked at arrival in case the
/// terrain changed or another sniffer already claimed it.
fn diggable_block_below(world: &ChunkWorld, walk_target: Vec3) -> bool {
    let x = walk_target.x.floor() as i32;
    let y = walk_target.y.floor() as i32 - 1;
    let z = walk_target.z.floor() as i32;
    DIGGABLE_BLOCKS.contains(&bare_block_id(world.block_state(x, y, z)))
}

impl<'w> MobSim<'w> {
    /// Issue #230's sniffer state machine — the per-tick driver for every
    /// live sniffer's `IDLING -> SNIFFING -> SEARCHING -> DIGGING -> RISING`
    /// loop. See this module's own doc for the full account and every
    /// disclosed narrowing.
    pub(super) fn tick_sniffers(&mut self) {
        let world = self.world;
        let tick_count = self.tick_count;
        let mut loot_drops: Vec<(i32, Vec3)> = Vec::new();
        for m in &mut self.mobs {
            if m.entity_type.path() != "sniffer" || m.health <= 0.0 {
                continue;
            }
            if m.sniffer_sniff_cooldown > 0 {
                m.sniffer_sniff_cooldown -= 1;
            }
            match m.sniffer_state {
                SnifferState::Idling => {
                    if m.sniffer_sniff_cooldown == 0
                        && !m.is_baby()
                        && eligible_to_sniff(m)
                        && sniff_roll(m.id, tick_count)
                    {
                        let mut rng = event_rng(m.id, tick_count, 1);
                        m.sniffer_state = SnifferState::Sniffing;
                        m.sniffer_state_ticks =
                            roll_duration(&mut rng, SNIFFING_MIN_TICKS, SNIFFING_MAX_TICKS);
                    }
                }
                SnifferState::Sniffing => {
                    if !eligible_to_sniff(m) {
                        m.sniffer_state = SnifferState::Idling;
                        continue;
                    }
                    m.sniffer_state_ticks -= 1;
                    if m.sniffer_state_ticks <= 0 {
                        // `Sniffing.stop`, `finished` branch: search for a
                        // dig position and start walking there, or fall
                        // back to `IDLING` when none is found.
                        match find_dig_position(world, m) {
                            Some(target) => {
                                m.sniffer_dig_target = Some(target);
                                m.sniffer_state = SnifferState::Searching;
                                m.sniffer_state_ticks = SEARCHING_TIMEOUT_TICKS;
                            }
                            None => m.sniffer_state = SnifferState::Idling,
                        }
                    }
                }
                SnifferState::Searching => {
                    if !eligible_to_sniff(m) {
                        m.sniffer_state = SnifferState::Idling;
                        m.sniffer_dig_target = None;
                        continue;
                    }
                    let Some(target) = m.sniffer_dig_target else {
                        // No target at all — nothing to walk toward or
                        // arrive at; fall back rather than loop forever.
                        m.sniffer_state = SnifferState::Idling;
                        continue;
                    };
                    if super::dist_sqr(m.position(), target) <= DIG_ARRIVAL_DISTANCE_SQR {
                        // `Searching.stop`: `if (canDig() && canSniff())
                        // setMemory(SNIFFER_DIGGING, true)`, re-checked here
                        // rather than trusted from the search above — the
                        // terrain (or another sniffer's own dig) may have
                        // changed in the time it took to walk over.
                        if eligible_to_dig(m) && diggable_block_below(world, target) {
                            m.sniffer_state = SnifferState::Digging;
                            let mut rng = event_rng(m.id, tick_count, 2);
                            m.sniffer_state_ticks =
                                roll_duration(&mut rng, DIGGING_MIN_TICKS, DIGGING_MAX_TICKS);
                        } else {
                            m.sniffer_state = SnifferState::Idling;
                        }
                        m.sniffer_dig_target = None;
                        continue;
                    }
                    m.sniffer_state_ticks -= 1;
                    if m.sniffer_state_ticks <= 0 {
                        m.sniffer_state = SnifferState::Idling;
                        m.sniffer_dig_target = None;
                    }
                }
                SnifferState::Digging => {
                    // Disclosed narrowing: only panic interrupts a dig —
                    // see the module doc.
                    if m.is_panicking() {
                        m.sniffer_state = SnifferState::Idling;
                        continue;
                    }
                    m.sniffer_state_ticks -= 1;
                    if m.sniffer_state_ticks <= 0 {
                        // `FinishedDigging.start` -> `RISING`;
                        // `onDiggingComplete(true)`'s explored-position
                        // record happens here rather than in `RISING`'s own
                        // stop, since this module has no separate
                        // "timed out vs finished" distinction to gate it on
                        // — every dig that reaches here completed for real.
                        loot_drops.push((m.id, m.position()));
                        m.sniffer_explored.insert(0, m.position());
                        m.sniffer_explored.truncate(EXPLORED_POSITIONS_CAP);
                        m.sniffer_state = SnifferState::Rising;
                        m.sniffer_state_ticks = RISING_TICKS;
                    }
                }
                SnifferState::Rising => {
                    m.sniffer_state_ticks -= 1;
                    if m.sniffer_state_ticks <= 0 {
                        m.sniffer_state = SnifferState::Idling;
                        m.sniffer_sniff_cooldown = SNIFF_COOLDOWN_TICKS;
                    }
                }
            }
        }

        if loot_drops.is_empty() {
            return;
        }
        // `Sniffer.dropSeed`/`dropFromGiftLootTable(SNIFFER_DIGGING, …)` —
        // the same loot-table-then-`spawn_item` shape `resolve_cat_gifts`
        // already uses, with the item spawned at the sniffer's own position
        // (disclosed — see the module doc's "no head-forward offset" cut).
        let table = ResourceKey::new("minecraft", "gameplay/sniffer_digging")
            .expect("a static loot-table key parses");
        let tables = crate::block_drops::bundled_tables();
        for (id, pos) in loot_drops {
            let mut rng = event_rng(id, tick_count, 3);
            let rolled = tables.roll(&table, &crate::loot::LootContext::default(), &mut rng);
            for stack in rolled {
                if stack.count == 0 {
                    continue;
                }
                let velocity = crate::block_drops::dropped_item_velocity(&mut rng);
                let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
                self.spawn_item(
                    stack.item.clone(),
                    pos,
                    velocity,
                    lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                        count,
                        lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::MetadataField;
    use lodestone_model::ResourceKey;
    use std::str::FromStr;

    /// A flat diggable floor (grass over dirt) wide enough for the search
    /// radius, with a player-sized clearing above.
    fn diggable_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -16..=16 {
            for z in -16..=16 {
                world.set_block(x, 0, z, "minecraft:grass_block");
            }
        }
        world
    }

    /// A world with no diggable block anywhere (plain stone) — the negative
    /// control for [`a_sniffer_left_alone_eventually_starts_sniffing_and_finds_a_dig_target`].
    fn undiggable_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -16..=16 {
            for z in -16..=16 {
                world.set_solid(x, 0, z, true);
            }
        }
        world
    }

    fn spawn_sniffer(world: &ChunkWorld) -> (MobSim<'_>, i32) {
        let mut sim = MobSim::new(world);
        let key = ResourceKey::from_str("minecraft:sniffer").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 1.0, 0.0)).id();
        (sim, id)
    }

    /// Real production path: a sniffer left alone long enough, over real
    /// diggable ground, eventually sniffs, finds a target, walks there
    /// (`Brain`'s own `WalkToPoi`, driven through `MobSim::tick`), digs, and
    /// drops real seed/pod loot — the whole state machine, not a single
    /// isolated transition.
    #[test]
    fn a_sniffer_left_alone_eventually_digs_and_drops_loot() {
        let world = diggable_world();
        let (mut sim, id) = spawn_sniffer(&world);

        let mut reached_digging = false;
        let mut item_seen = false;
        for _ in 0..30_000 {
            sim.tick();
            let mob = sim.get(id).expect("alive");
            if mob.sniffer_state == SnifferState::Digging {
                reached_digging = true;
            }
            if sim.item_count() > 0 {
                item_seen = true;
                break;
            }
        }
        assert!(reached_digging, "a sniffer over real diggable ground must eventually start digging");
        assert!(item_seen, "a finished dig must drop real loot (torchflower seeds or a pitcher pod)");
    }

    /// **Control**: the identical wait with no diggable block anywhere in
    /// range. The sniffer may still sniff (that needs no world read), but it
    /// must never find a target or reach `DIGGING` — proving the diggable-
    /// block search itself, not merely the timer, gates digging.
    #[test]
    fn a_sniffer_over_undiggable_ground_never_digs() {
        let world = undiggable_world();
        let (mut sim, id) = spawn_sniffer(&world);

        for _ in 0..5_000 {
            sim.tick();
            assert_ne!(
                sim.get(id).expect("alive").sniffer_state,
                SnifferState::Digging,
                "no diggable block exists anywhere near this sniffer"
            );
        }
    }

    /// The wire metadata reports the real state ordinal, not a placeholder —
    /// checked at `IDLING` (the reachable-in-5-ticks baseline) and via
    /// [`SnifferState::wire_ordinal`]'s own table for every state this crate
    /// produces.
    #[test]
    fn snapshot_carries_the_real_sniffer_state_ordinal() {
        let world = diggable_world();
        let (mut sim, id) = spawn_sniffer(&world);
        sim.tick();
        assert!(
            sim.get(id)
                .expect("alive")
                .snapshot()
                .metadata
                .contains(&MetadataField::SnifferState(0)),
            "a fresh sniffer must report Sniffer.State.IDLING (0)"
        );
        assert_eq!(SnifferState::Sniffing.wire_ordinal(), 3, "Sniffer.State.SNIFFING");
        assert_eq!(SnifferState::Searching.wire_ordinal(), 4, "Sniffer.State.SEARCHING");
        assert_eq!(SnifferState::Digging.wire_ordinal(), 5, "Sniffer.State.DIGGING");
        assert_eq!(SnifferState::Rising.wire_ordinal(), 6, "Sniffer.State.RISING");
    }

    /// **Control**: a cow, run through the identical wait, must never report
    /// a `SnifferState` field at all — proving the metadata push is
    /// species-gated, not a field every mob happens to share.
    #[test]
    fn only_a_sniffer_ever_reports_a_sniffer_state_a_cow_never_does() {
        let world = diggable_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:cow").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 1.0, 0.0)).id();
        for _ in 0..200 {
            sim.tick();
            assert!(
                !sim
                    .get(id)
                    .expect("alive")
                    .snapshot()
                    .metadata
                    .iter()
                    .any(|f| matches!(f, MetadataField::SnifferState(_))),
                "a cow must never carry a SnifferState field"
            );
        }
    }
}
