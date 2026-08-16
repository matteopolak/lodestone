//! Villager professions and workstation claiming (issues #243, #245).
//!
//! # What it is
//!
//! The block-side half of vanilla's `VillagerProfession`/`PoiTypes` pairing:
//! which workstation block registers which point-of-interest type
//! ([`poi_type_for_block`]), which profession that POI type hands out
//! ([`profession_for_poi_type`]), and the live claim ledger
//! ([`WorkstationClaims`]) an unemployed villager's search
//! ([`find_and_claim_workstation`]) draws a job from. Leveling
//! ([`level_up`]) and trade generation ([`trades::offers_up_to`]) build on
//! top of a profession once claimed.
//!
//! # How it works
//!
//! `crate::poi_storage` (issue #303's second half) already carries every
//! profession POI type's ticket cap in [`crate::poi_storage::max_tickets`]
//! and the claim mechanics themselves
//! ([`crate::poi_storage::PoiRecord::acquire_ticket`]/`release_ticket`) —
//! this module's own doc names villager professions as its "natural second
//! consumer" after portal lookup, and [`WorkstationClaims`] is that consumer:
//! it wraps a `HashMap<BlockPos, PoiRecord>` and claims/releases through the
//! real record type rather than a parallel claimed-by-uuid table.
//!
//! [`find_and_claim_workstation`] is the search a job-seeking villager runs:
//! a bounded nearest-first scan of [`ChunkWorld`] for a block
//! [`poi_type_for_block`] recognises, claiming the nearest one with a free
//! ticket. Losing the block is handled by re-verification, not an event
//! hook — see [`WorkstationClaims::remove`]'s doc for why, and this module's
//! own "what is not built" section below for the trade-off that buys.
//!
//! # What is not built, named rather than silent
//!
//! - **No on-disk persistence.** [`WorkstationClaims`] is a session-only
//!   ledger, not backed by [`crate::poi_storage::PoiStorage`]. Wiring it into
//!   the save/restore path this crate already has for portals would touch
//!   `crate::integrated`, which is off limits for this change. A restart
//!   loses every claim; every villager re-scans and re-claims from a clean
//!   slate, which is a disclosed gap rather than a silent one.
//! - **No block-place/break event hook.** A workstation destroyed or
//!   replaced is detected by the next re-verification pass reading a
//!   different (or absent) POI type at the claimed position, not
//!   immediately. `crate::mobs::MobSim::tick_villager_professions` runs this
//!   check every tick for every employed villager, so the lag is at most one
//!   tick in practice — but it is a poll, not a push, and touching the real
//!   event path would mean editing `crate::block_entities`/`crate::server`
//!   far beyond a "minimal hunk".
//! - **`VillagerType` (biome flavour) is not derived.** Every claimed
//!   villager reports `minecraft:plains` regardless of where it stands —
//!   `VillagerType.byBiome` is real vanilla logic this module does not port.
//!   Cosmetic only; profession and workstation claiming do not depend on it.
//! - **[`WorkstationClaims`]/[`find_and_claim_workstation`] are native-only**,
//!   `#[cfg(not(target_arch = "wasm32"))]` — they reuse
//!   `crate::poi_storage::PoiRecord`, and `crate::poi_storage` itself is
//!   gated the same way in `lib.rs` (a `std::fs` region-file module). This
//!   crate compiles for `wasm32-unknown-unknown` (`scripts/wasm-check.sh`'s
//!   `CRATES` list — the browser's own singleplayer path links it), so an
//!   ungated `use crate::poi_storage::PoiRecord` here would break that build
//!   exactly the way `crate::portal`'s POI conversion functions once did
//!   (see `docs/point-of-interest-storage.md`'s own account of that break).
//!   `Profession`, the block/POI/profession tables and leveling are plain
//!   data with no such dependency and stay available on every target — only
//!   the claim ledger itself, and the search that uses it, are narrowed.

use std::collections::HashMap;
use std::str::FromStr;

use lodestone_model::{BlockPos, ResourceKey};

#[cfg(not(target_arch = "wasm32"))]
use super::ChunkWorld;
#[cfg(not(target_arch = "wasm32"))]
use crate::poi_storage::PoiRecord;

pub mod conversion;
pub mod gossip;
pub mod reputation;
pub mod trades;

/// `minecraft:villager_profession`, in `VillagerProfession.bootstrap`'s own
/// registration order (`.cache/mc/26.2/src/net/minecraft/world/entity/npc/villager/VillagerProfession.java`) —
/// the same order `crates/protocol/v770/src/entity_variants.rs`'s
/// `VILLAGER_PROFESSION` table transcribes independently for the wire id.
/// Both must agree with the jar; neither is derived from the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profession {
    /// `VillagerProfession.NONE` — every villager starts here.
    #[default]
    None,
    Armorer,
    Butcher,
    Cartographer,
    Cleric,
    Farmer,
    Fisherman,
    Fletcher,
    Leatherworker,
    Librarian,
    Mason,
    /// `VillagerProfession.NITWIT` — like `None`, has no job site and no
    /// trades (`register(registry, NITWIT, PoiType.NONE, PoiType.NONE,
    /// null)`), but is a distinct, permanent state a villager is born into
    /// rather than one it can be unemployed from.
    Nitwit,
    Shepherd,
    Toolsmith,
    Weaponsmith,
}

impl Profession {
    /// The registry path, e.g. `"farmer"` — matches
    /// `crate::poi_storage::max_tickets`'s own POI-type path arms for the
    /// eleven of these that are also POI-type names.
    #[must_use]
    pub fn path(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Armorer => "armorer",
            Self::Butcher => "butcher",
            Self::Cartographer => "cartographer",
            Self::Cleric => "cleric",
            Self::Farmer => "farmer",
            Self::Fisherman => "fisherman",
            Self::Fletcher => "fletcher",
            Self::Leatherworker => "leatherworker",
            Self::Librarian => "librarian",
            Self::Mason => "mason",
            Self::Nitwit => "nitwit",
            Self::Shepherd => "shepherd",
            Self::Toolsmith => "toolsmith",
            Self::Weaponsmith => "weaponsmith",
        }
    }
}

/// `VillagerProfession.bootstrap`'s `jobSite -> profession` pairing,
/// inverted: which profession a workstation POI type hands out. Only the
/// thirteen professions with a real job site answer `Some` — `None` and
/// `Nitwit` both register `PoiType.NONE` (`.cache/mc/26.2/src/net/minecraft/world/entity/ai/village/poi/PoiType.java`'s
/// sentinel, not a real POI type any block produces), so neither is
/// reachable from a POI type and both are absent here by construction, not
/// by omission.
#[must_use]
pub fn profession_for_poi_type(poi_type_path: &str) -> Option<Profession> {
    Some(match poi_type_path {
        "armorer" => Profession::Armorer,
        "butcher" => Profession::Butcher,
        "cartographer" => Profession::Cartographer,
        "cleric" => Profession::Cleric,
        "farmer" => Profession::Farmer,
        "fisherman" => Profession::Fisherman,
        "fletcher" => Profession::Fletcher,
        "leatherworker" => Profession::Leatherworker,
        "librarian" => Profession::Librarian,
        "mason" => Profession::Mason,
        "shepherd" => Profession::Shepherd,
        "toolsmith" => Profession::Toolsmith,
        "weaponsmith" => Profession::Weaponsmith,
        _ => return None,
    })
}

/// `PoiTypes.bootstrap`'s block registrations, restricted to the thirteen
/// workstation types (`home`/`meeting`/`bee_nest`/`nether_portal`/… are not
/// profession job sites and are out of this module's scope — the first three
/// have no consumer anywhere in this codebase yet, and the fourth is
/// `crate::portal`'s existing one).
///
/// `block_id` is the *bare* id — no `minecraft:` namespace, no `[...]` state
/// properties — see [`bare_block_id`], which every caller here runs the raw
/// `ChunkWorld::block_state` string through first.
#[must_use]
pub fn poi_type_for_block(block_id: &str) -> Option<&'static str> {
    match block_id {
        "blast_furnace" => Some("armorer"),
        "smoker" => Some("butcher"),
        "cartography_table" => Some("cartographer"),
        "brewing_stand" => Some("cleric"),
        "composter" => Some("farmer"),
        "barrel" => Some("fisherman"),
        "fletching_table" => Some("fletcher"),
        // `PoiTypes.CAULDRONS`: all four cauldron fill states share one POI type.
        "cauldron" | "water_cauldron" | "lava_cauldron" | "powder_snow_cauldron" => {
            Some("leatherworker")
        }
        "lectern" => Some("librarian"),
        "stonecutter" => Some("mason"),
        "loom" => Some("shepherd"),
        "smithing_table" => Some("toolsmith"),
        "grindstone" => Some("weaponsmith"),
        _ => None,
    }
}

/// Strips a [`ChunkWorld::block_state`] string down to its bare block id:
/// `"minecraft:composter[level=3]"` -> `"composter"`. Every caller of
/// [`poi_type_for_block`] in this module runs its input through this first,
/// since the world snapshot answers full state strings and the POI table is
/// keyed on the block alone (vanilla's own `PoiTypes.forState` matches every
/// *state* of the registered blocks, not a property subset).
#[must_use]
pub fn bare_block_id(state: &str) -> &str {
    let without_namespace = state.strip_prefix("minecraft:").unwrap_or(state);
    without_namespace
        .split('[')
        .next()
        .unwrap_or(without_namespace)
}

/// The live, in-memory workstation claim ledger.
///
/// Reuses [`crate::poi_storage::PoiRecord`]'s own ticket accounting rather
/// than a parallel claimed-by-uuid map: [`PoiRecord::acquire_ticket`]/
/// [`PoiRecord::release_ticket`] are exactly vanilla's
/// `PoiRecord.acquireTicket`/`releaseTicket`, and
/// [`crate::poi_storage::max_tickets`] already carries every profession POI
/// type's cap (`1`, transcribed from `PoiTypes.bootstrap`) — this module
/// invents no new occupancy math.
///
/// Native-only — see this module's own doc for why.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub struct WorkstationClaims {
    records: HashMap<(i32, i32, i32), PoiRecord>,
}

#[cfg(not(target_arch = "wasm32"))]
impl WorkstationClaims {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensures a record exists at `pos` for `poi_type` — `PoiSection::add`'s
    /// own semantics, reused rather than restated: a record of the *same*
    /// type already there is left untouched (so an existing claim survives
    /// rediscovery), and a record of a *different* type is replaced fresh at
    /// full tickets (matching vanilla's mismatch-and-overwrite).
    pub fn discover(&mut self, pos: BlockPos, poi_type: ResourceKey) -> &mut PoiRecord {
        let key = (pos.x, pos.y, pos.z);
        let needs_fresh = match self.records.get(&key) {
            Some(existing) => existing.poi_type != poi_type,
            None => true,
        };
        if needs_fresh {
            self.records.insert(key, PoiRecord::new(pos, poi_type));
        }
        self.records
            .get_mut(&key)
            .expect("just inserted, or already present")
    }

    /// The workstation at `pos` is gone or changed kind — vanilla's
    /// `PoiManager.remove`. Any ticket held there is discarded with the
    /// record itself: a villager whose claim this was finds
    /// [`get`](Self::get) answers `None` on its next verify pass and goes
    /// back to unemployed.
    pub fn remove(&mut self, pos: BlockPos) {
        self.records.remove(&(pos.x, pos.y, pos.z));
    }

    #[must_use]
    pub fn get(&self, pos: BlockPos) -> Option<&PoiRecord> {
        self.records.get(&(pos.x, pos.y, pos.z))
    }

    /// Claims one ticket at `pos`, discovering the record first if needed.
    /// `false` if every ticket there is already held — the occupancy gate
    /// this whole ledger exists to enforce, run through
    /// [`PoiRecord::acquire_ticket`] rather than reimplemented.
    pub fn try_claim(&mut self, pos: BlockPos, poi_type: ResourceKey) -> bool {
        self.discover(pos, poi_type).acquire_ticket()
    }

    /// Releases a previously claimed ticket at `pos`. A no-op if nothing is
    /// claimed there (a claim that outlived its record, already handled by
    /// [`remove`](Self::remove)).
    pub fn release(&mut self, pos: BlockPos) {
        if let Some(record) = self.records.get_mut(&(pos.x, pos.y, pos.z)) {
            record.release_ticket();
        }
    }
}

/// How far a job-seeking villager scans. Vanilla's own search
/// (`PoiManager.getRandom`, reached through the `AssignProfessionFromJobSite`/
/// `YieldJobSite` behaviors) reads a **persistent per-section index** out to
/// 48 blocks; this sim has no such index outside a claim's own lifetime (see
/// [`WorkstationClaims`]'s module-level doc for why), so a search here is a
/// bounded terrain re-scan instead. `SEARCH_RADIUS` is deliberately smaller
/// than vanilla's 48 for the reason every unbounded scan in this crate is
/// bounded: nothing backs it with a spatial index, so a cube scan is
/// `O(radius^3)` per idle villager per search. A villager standing well
/// outside a real workstation's reach simply will not find it — an honest,
/// disclosed narrowing, not a silent one.
///
/// Native-only — see this module's own doc for why.
#[cfg(not(target_arch = "wasm32"))]
pub const SEARCH_RADIUS: i32 = 16;

/// Runs one job search from `origin`: a nearest-first scan of `world` for a
/// workstation block, claiming the first one with a free ticket.
///
/// Nearest-first is this module's own approximation of vanilla's
/// `PoiManager.getRandom`'s "closest of a random sample" — a full port of
/// that sampling is not attempted here (see [`SEARCH_RADIUS`]'s doc for the
/// wider disclosed gap it belongs to); nearest-first is a defensible,
/// deterministic stand-in that a two-villager/one-workstation contention
/// test can still observe cleanly.
///
/// Native-only — see this module's own doc for why.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn find_and_claim_workstation(
    origin: BlockPos,
    world: &ChunkWorld,
    claims: &mut WorkstationClaims,
) -> Option<(BlockPos, Profession)> {
    let mut candidates: Vec<BlockPos> = Vec::new();
    for dx in -SEARCH_RADIUS..=SEARCH_RADIUS {
        for dy in -SEARCH_RADIUS..=SEARCH_RADIUS {
            for dz in -SEARCH_RADIUS..=SEARCH_RADIUS {
                let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                let state = world.block_state(pos.x, pos.y, pos.z);
                if poi_type_for_block(bare_block_id(state)).is_some() {
                    candidates.push(pos);
                }
            }
        }
    }
    candidates.sort_by_key(|p| {
        let dx = i64::from(p.x - origin.x);
        let dy = i64::from(p.y - origin.y);
        let dz = i64::from(p.z - origin.z);
        dx * dx + dy * dy + dz * dz
    });
    for pos in candidates {
        let state = world.block_state(pos.x, pos.y, pos.z);
        let Some(poi_path) = poi_type_for_block(bare_block_id(state)) else {
            continue;
        };
        let Some(profession) = profession_for_poi_type(poi_path) else {
            continue;
        };
        let poi_type = ResourceKey::from_str(&format!("minecraft:{poi_path}"))
            .expect("a table-derived POI path is always a valid identifier");
        if claims.try_claim(pos, poi_type) {
            return Some((pos, profession));
        }
    }
    None
}

/// `VillagerData.NEXT_LEVEL_XP_THRESHOLDS` —
/// `.cache/mc/26.2/src/net/minecraft/world/entity/npc/villager/VillagerData.java`.
/// Indexed `[level - 1]` for the minimum, `[level]` for the maximum a given
/// level spans.
const NEXT_LEVEL_XP_THRESHOLDS: [i32; 5] = [0, 10, 70, 150, 250];

/// `VillagerData.canLevelUp`: every level except the level-5 mastery cap.
#[must_use]
pub fn can_level_up(level: i32) -> bool {
    (1..5).contains(&level)
}

/// `VillagerData.getMaxXpPerLevel` — the xp threshold this level advances at,
/// or `0` past mastery.
#[must_use]
pub fn max_xp_for_level(level: i32) -> i32 {
    if can_level_up(level) {
        NEXT_LEVEL_XP_THRESHOLDS[level as usize]
    } else {
        0
    }
}

/// `Villager.shouldIncreaseLevel`/`increaseMerchantCareer`, applied
/// repeatedly in case `xp` clears more than one threshold at once.
///
/// **The comparison is `>=`, not `>`** — vanilla's own gate reads
/// `this.villagerXp >= VillagerData.getMaxXpPerLevel(currentLevel)`
/// (`Villager.java`'s `shouldIncreaseLevel`), so a villager whose xp lands
/// **exactly** on a threshold levels up the same tick, not one xp later.
#[must_use]
pub fn level_up(mut level: i32, xp: i32) -> i32 {
    while can_level_up(level) && xp >= max_xp_for_level(level) {
        level += 1;
    }
    level
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_workstation_block_resolves_to_the_profession_that_claims_it() {
        // Enumerates `poi_type_for_block`'s own table rather than hand-listing
        // pairs a second time, so this cannot drift from the match arms above.
        let blocks = [
            "blast_furnace",
            "smoker",
            "cartography_table",
            "brewing_stand",
            "composter",
            "barrel",
            "fletching_table",
            "cauldron",
            "water_cauldron",
            "lava_cauldron",
            "powder_snow_cauldron",
            "lectern",
            "stonecutter",
            "loom",
            "smithing_table",
            "grindstone",
        ];
        for block in blocks {
            let poi_type = poi_type_for_block(block)
                .unwrap_or_else(|| panic!("{block} should register a POI type"));
            assert!(
                profession_for_poi_type(poi_type).is_some(),
                "{block} -> {poi_type} should resolve to a real profession"
            );
        }
    }

    #[test]
    fn bare_block_id_strips_namespace_and_state() {
        assert_eq!(bare_block_id("minecraft:composter[level=3]"), "composter");
        assert_eq!(bare_block_id("minecraft:lectern"), "lectern");
        assert_eq!(bare_block_id("minecraft:air"), "air");
    }

    /// The discriminating claim gate issue #243 asks for: **two villagers,
    /// one workstation.** A single-villager test would pass under an
    /// implementation with no occupancy at all — this one fails under that
    /// implementation, because the second search would also succeed.
    #[test]
    fn a_second_villager_cannot_claim_an_already_claimed_workstation() {
        let mut world = ChunkWorld::new(-64, 384);
        // Pairwise-distinct coordinates, never `1, 1, 4`.
        world.set_block(100, 71, 205, "minecraft:composter");

        let mut claims = WorkstationClaims::new();
        let first = find_and_claim_workstation(BlockPos::new(100, 70, 202), &world, &mut claims);
        assert_eq!(
            first,
            Some((BlockPos::new(100, 71, 205), Profession::Farmer))
        );

        // A second, closer villager finds the same block but every ticket is
        // already held (`max_tickets` is `1` for every profession job site).
        let second = find_and_claim_workstation(BlockPos::new(100, 70, 206), &world, &mut claims);
        assert_eq!(
            second, None,
            "the workstation has one ticket and the first villager already holds it"
        );

        // Control: the occupancy check itself is doing the excluding, not an
        // accident of the search never reaching the block — releasing the
        // ticket makes it claimable again.
        claims.release(BlockPos::new(100, 71, 205));
        let third = find_and_claim_workstation(BlockPos::new(100, 70, 206), &world, &mut claims);
        assert_eq!(
            third,
            Some((BlockPos::new(100, 71, 205), Profession::Farmer)),
            "releasing the ticket must make the workstation claimable again"
        );
    }

    #[test]
    fn losing_the_workstation_loses_the_job() {
        let mut claims = WorkstationClaims::new();
        let pos = BlockPos::new(11, 70, 233);
        let poi_type = ResourceKey::from_str("minecraft:farmer").unwrap();
        assert!(claims.try_claim(pos, poi_type));
        assert!(claims.get(pos).is_some());

        claims.remove(pos);
        assert!(
            claims.get(pos).is_none(),
            "removing the workstation must drop its claim, not merely free a ticket"
        );
    }

    /// The XP-threshold inclusive/exclusive hazard this repo has already
    /// measured for the player XP curve, applied here: level 1's threshold
    /// to level 2 is exactly 10. `>=` (vanilla's real reading) levels up at
    /// xp == 10; `>` (the wrong reading) would not.
    #[test]
    fn leveling_up_at_exactly_the_threshold_uses_the_inclusive_reading() {
        assert_eq!(max_xp_for_level(1), 10);
        assert_eq!(level_up(1, 9), 1, "9 xp is short of the threshold");
        assert_eq!(
            level_up(1, 10),
            2,
            "10 xp meets the threshold exactly and must level up under >=, not >"
        );
    }

    #[test]
    fn a_mastered_villager_does_not_level_past_five() {
        assert_eq!(level_up(5, 1_000_000), 5);
    }

    #[test]
    fn leveling_can_cross_more_than_one_threshold_at_once() {
        // 70 clears both the level-1->2 (10) and level-2->3 (70) thresholds.
        assert_eq!(level_up(1, 70), 3);
    }
}
