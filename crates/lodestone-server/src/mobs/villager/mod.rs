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
//!
//! # Bed claiming ([`BedClaims`]) is a sibling of the above, not a new shape
//!
//! `PoiTypes.HOME` (`minecraft:home`, `#minecraft:beds` -> one ticket per
//! bed) is a POI type this module's own `max_tickets` already priced, but
//! nothing ever claimed one — issue #241 (raids)'s own trigger,
//! `Raids.createOrExtendRaid`, needs an *occupied* `#village` POI (home,
//! meeting or a job site) within 64 blocks before it will start a raid at
//! all, and a bed that is never claimed can never be occupied.
//! [`BedClaims`]/[`find_and_claim_bed`] are exactly [`WorkstationClaims`]/
//! [`find_and_claim_workstation`]'s shape, reused rather than reinvented:
//! same ticket accounting through `PoiRecord`, same bounded nearest-first
//! terrain scan, same disclosed gaps (no on-disk persistence, no
//! block-event hook, native-only). The one real difference from a job site
//! is vanilla's own `validateBedPoi` — a bed currently `occupied=true` (someone
//! is physically asleep in it right now) is skipped even if a ticket is
//! free, which [`find_and_claim_bed`] ports directly.
//!
//! **This claims the bed as a *ticket*, not as a nightly sleep.** Vanilla's
//! `PoiRecord.isOccupied` (which `Raids.createOrExtendRaid`'s
//! `Occupancy.IS_OCCUPIED` query reads) is true the moment a villager's
//! `AcquirePoi` behavior takes the ticket — independent of whether anyone is
//! ever physically lying in the bed. So the raid trigger's occupancy check
//! needs only this claim, not a full work/rest sleep cycle (`SleepInBed`,
//! `LAST_SLEPT`, issue #231's own remainder) — that is a real, separate gap
//! this module does not close.
//!
//! # Bell claiming ([`BellClaims`]) is the third sibling, and it is what feeds `MEET`
//!
//! [`BellClaims`]/[`find_and_claim_bell`] complete the `#village` POI trio
//! (workstation, bed, bell) with the identical ticket-accounting shape, this
//! time against `PoiTypes.MEETING`'s 32-ticket cap. Nothing about the raid
//! trigger needs a bell specifically (`occupied_homes_in_range` already
//! covers that with beds alone, matching vanilla's own `#village` tag
//! query), but issue #231's WORK/MEET/REST schedule
//! (`crates/lodestone-entity`'s `brain::roster::villager_brain`) does: `MEET`
//! is only eligible once `MemoryModuleType::MEETING_POINT` holds a value,
//! and nothing wrote that memory until this. `crate::mobs::MobSim::tick_villager_bells`/
//! `meeting_point` are this ledger's production wiring, the same shape
//! [`tick_villager_beds`](crate::mobs::MobSim::tick_villager_beds) already is.

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

/// `BlockTags.BEDS` — every one of the sixteen dyed bed blocks, all
/// registering `PoiTypes.HOME` (`PoiTypes.bootstrap`'s
/// `register(HOME, BlockTags.BEDS, 1, 1)`). Takes a *bare* block id, the
/// same contract [`poi_type_for_block`] has — run a raw
/// [`ChunkWorld::block_state`] string through [`bare_block_id`] first.
#[must_use]
pub fn is_bed_block(block_id: &str) -> bool {
    matches!(
        block_id,
        "white_bed"
            | "orange_bed"
            | "magenta_bed"
            | "light_blue_bed"
            | "yellow_bed"
            | "lime_bed"
            | "pink_bed"
            | "gray_bed"
            | "light_gray_bed"
            | "cyan_bed"
            | "purple_bed"
            | "blue_bed"
            | "brown_bed"
            | "green_bed"
            | "red_bed"
            | "black_bed"
    )
}

/// `VillagerGoalPackages.validateBedPoi`'s block-state half: `true` when a
/// *full* (not bare) state string carries `occupied=true` — someone is
/// physically asleep in this bed right now, distinct from
/// [`PoiRecord::is_occupied`]'s ticket-based sense of "occupied", which this
/// module's own claiming logic never sets from this check.
#[must_use]
fn bed_state_is_occupied(state: &str) -> bool {
    state.contains("occupied=true")
}

/// The `minecraft:home` [`ResourceKey`] every claimed bed is recorded under.
fn home_poi_type() -> ResourceKey {
    ResourceKey::from_str("minecraft:home").expect("a literal POI path is always valid")
}

/// The live, in-memory bed claim ledger — [`WorkstationClaims`]'s sibling
/// for `PoiTypes.HOME`. See this module's own "Bed claiming" doc section for
/// why this exists and what it deliberately does not model.
///
/// Native-only, for the identical reason [`WorkstationClaims`] is — see that
/// type's own doc.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub struct BedClaims {
    records: HashMap<(i32, i32, i32), PoiRecord>,
}

#[cfg(not(target_arch = "wasm32"))]
impl BedClaims {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensures a `home` record exists at `pos` — [`WorkstationClaims::discover`]'s
    /// own semantics, reused rather than restated.
    fn discover(&mut self, pos: BlockPos) -> &mut PoiRecord {
        let key = (pos.x, pos.y, pos.z);
        self.records
            .entry(key)
            .or_insert_with(|| PoiRecord::new(pos, home_poi_type()))
    }

    /// The bed at `pos` is gone or no longer a bed — vanilla's
    /// `PoiManager.remove`. Any ticket held there is discarded with the
    /// record itself.
    pub fn remove(&mut self, pos: BlockPos) {
        self.records.remove(&(pos.x, pos.y, pos.z));
    }

    #[must_use]
    pub fn get(&self, pos: BlockPos) -> Option<&PoiRecord> {
        self.records.get(&(pos.x, pos.y, pos.z))
    }

    /// Claims one ticket at `pos`, discovering the record first if needed.
    /// `false` if the bed's one ticket is already held.
    pub fn try_claim(&mut self, pos: BlockPos) -> bool {
        self.discover(pos).acquire_ticket()
    }

    /// Releases a previously claimed ticket at `pos`. A no-op if nothing is
    /// claimed there.
    pub fn release(&mut self, pos: BlockPos) {
        if let Some(record) = self.records.get_mut(&(pos.x, pos.y, pos.z)) {
            record.release_ticket();
        }
    }

    /// The live equivalent of
    /// [`crate::poi_storage::PoiStorage::occupied_in_range`], scoped to this
    /// ledger's own claimed beds: every claimed (ticket-occupied) bed within
    /// `radius` real blocks of `center`. Issue #241's raid trigger is this
    /// method's reason to exist — the on-disk `poi/` region set is never
    /// written for a bed claimed through this ledger (see this module's "No
    /// on-disk persistence" gap), so a live query against the ledger itself,
    /// not a disk read, is how a claimed bed is actually found today.
    #[must_use]
    pub fn occupied_in_range(&self, center: BlockPos, radius: i32) -> Vec<BlockPos> {
        let radius_sq = i64::from(radius) * i64::from(radius);
        self.records
            .values()
            .filter(|record| record.is_occupied())
            .map(|record| record.pos)
            .filter(|pos| {
                let dx = i64::from(pos.x - center.x);
                let dy = i64::from(pos.y - center.y);
                let dz = i64::from(pos.z - center.z);
                dx * dx + dy * dy + dz * dz <= radius_sq
            })
            .collect()
    }
}

/// Runs one bed search from `origin`: a nearest-first scan of `world` for an
/// unoccupied, unclaimed bed within [`SEARCH_RADIUS`], claiming the first
/// one found.
///
/// Ports `AcquirePoi.create(p -> p.is(PoiTypes.HOME), MemoryModuleType.HOME,
/// false, Optional.of((byte) 14), VillagerGoalPackages::validateBedPoi)`:
/// the `validPoi` predicate — skip a bed with `occupied=true` right now — is
/// [`bed_state_is_occupied`]; the ticket gate is [`BedClaims::try_claim`].
/// Nearest-first and [`SEARCH_RADIUS`] are [`find_and_claim_workstation`]'s
/// own disclosed narrowing, reused rather than restated — see that
/// function's doc.
///
/// Native-only — see this module's own doc for why.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn find_and_claim_bed(origin: BlockPos, world: &ChunkWorld, claims: &mut BedClaims) -> Option<BlockPos> {
    let mut candidates: Vec<BlockPos> = Vec::new();
    for dx in -SEARCH_RADIUS..=SEARCH_RADIUS {
        for dy in -SEARCH_RADIUS..=SEARCH_RADIUS {
            for dz in -SEARCH_RADIUS..=SEARCH_RADIUS {
                let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                let state = world.block_state(pos.x, pos.y, pos.z);
                if is_bed_block(bare_block_id(state)) {
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
        if !is_bed_block(bare_block_id(state)) {
            continue;
        }
        if bed_state_is_occupied(state) {
            continue;
        }
        if claims.try_claim(pos) {
            return Some(pos);
        }
    }
    None
}

/// `PoiTypes.MEETING`'s one registering block (`PoiTypes.bootstrap`'s
/// `register(MEETING, ImmutableSet.of(Blocks.BELL), 32, 1)`) — a single block
/// id, unlike [`is_bed_block`]'s sixteen-way tag match. Bare id, same
/// contract as [`poi_type_for_block`]/[`is_bed_block`].
#[must_use]
pub fn is_bell_block(block_id: &str) -> bool {
    block_id == "bell"
}

/// The `minecraft:meeting` [`ResourceKey`] every claimed bell is recorded
/// under — [`home_poi_type`]'s sibling.
fn meeting_poi_type() -> ResourceKey {
    ResourceKey::from_str("minecraft:meeting").expect("a literal POI path is always valid")
}

/// The live, in-memory bell claim ledger — [`WorkstationClaims`]/[`BedClaims`]'s
/// third sibling, for `PoiTypes.MEETING`. Where a workstation and a bed each
/// hand out one ticket, a bell hands out
/// [`crate::poi_storage::max_tickets`]'s `32` — vanilla villagers gather at a
/// bell in a crowd, not a queue of one.
///
/// Feeds [`crate::mobs::villager::MobSim::meeting_point`] no differently from
/// how [`BedClaims`] feeds [`MobSim::occupied_homes_in_range`] — a claimed
/// bell is what makes a `MEET`-activity villager (`crate::brain::roster::
/// villager_brain`'s schedule) have anywhere to walk. Native-only, for
/// [`WorkstationClaims`]'s own reason.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug, Default)]
pub struct BellClaims {
    records: HashMap<(i32, i32, i32), PoiRecord>,
}

#[cfg(not(target_arch = "wasm32"))]
impl BellClaims {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Ensures a `meeting` record exists at `pos` — [`WorkstationClaims::discover`]'s
    /// own semantics, reused rather than restated.
    fn discover(&mut self, pos: BlockPos) -> &mut PoiRecord {
        let key = (pos.x, pos.y, pos.z);
        self.records
            .entry(key)
            .or_insert_with(|| PoiRecord::new(pos, meeting_poi_type()))
    }

    /// The bell at `pos` is gone or no longer a bell — vanilla's
    /// `PoiManager.remove`. Any tickets held there are discarded with the
    /// record itself.
    pub fn remove(&mut self, pos: BlockPos) {
        self.records.remove(&(pos.x, pos.y, pos.z));
    }

    #[must_use]
    pub fn get(&self, pos: BlockPos) -> Option<&PoiRecord> {
        self.records.get(&(pos.x, pos.y, pos.z))
    }

    /// Claims one of the bell's 32 tickets at `pos`, discovering the record
    /// first if needed. `false` only once all 32 are held.
    pub fn try_claim(&mut self, pos: BlockPos) -> bool {
        self.discover(pos).acquire_ticket()
    }

    /// Releases a previously claimed ticket at `pos`. A no-op if nothing is
    /// claimed there.
    pub fn release(&mut self, pos: BlockPos) {
        if let Some(record) = self.records.get_mut(&(pos.x, pos.y, pos.z)) {
            record.release_ticket();
        }
    }
}

/// Runs one bell search from `origin`: a nearest-first scan of `world` for a
/// bell with a free ticket, claiming the first one found —
/// [`find_and_claim_bed`]'s own shape, restricted to [`is_bell_block`] and
/// with no `validateBedPoi`-equivalent extra check (a bell has no "someone is
/// using it right now" state the way a bed does).
///
/// Native-only — see this module's own doc for why.
#[cfg(not(target_arch = "wasm32"))]
#[must_use]
pub fn find_and_claim_bell(origin: BlockPos, world: &ChunkWorld, claims: &mut BellClaims) -> Option<BlockPos> {
    let mut candidates: Vec<BlockPos> = Vec::new();
    for dx in -SEARCH_RADIUS..=SEARCH_RADIUS {
        for dy in -SEARCH_RADIUS..=SEARCH_RADIUS {
            for dz in -SEARCH_RADIUS..=SEARCH_RADIUS {
                let pos = BlockPos::new(origin.x + dx, origin.y + dy, origin.z + dz);
                let state = world.block_state(pos.x, pos.y, pos.z);
                if is_bell_block(bare_block_id(state)) {
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
        if !is_bell_block(bare_block_id(state)) {
            continue;
        }
        if claims.try_claim(pos) {
            return Some(pos);
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
    fn every_bed_colour_registers_as_a_home_poi() {
        // Enumerates the sixteen dyed bed blocks explicitly, mirroring the
        // workstation table's own enumeration test above rather than
        // deriving the list from `is_bed_block` itself.
        let beds = [
            "white_bed",
            "orange_bed",
            "magenta_bed",
            "light_blue_bed",
            "yellow_bed",
            "lime_bed",
            "pink_bed",
            "gray_bed",
            "light_gray_bed",
            "cyan_bed",
            "purple_bed",
            "blue_bed",
            "brown_bed",
            "green_bed",
            "red_bed",
            "black_bed",
        ];
        for bed in beds {
            assert!(is_bed_block(bed), "{bed} should register as a home POI");
        }
        assert!(!is_bed_block("composter"), "a workstation is not a bed");
        assert!(!is_bed_block("air"), "air is not a bed");
    }

    /// The same discriminating shape as
    /// [`a_second_villager_cannot_claim_an_already_claimed_workstation`]: two
    /// villagers, one bed. A single-villager test would pass with no
    /// occupancy modelled at all.
    #[test]
    fn a_second_villager_cannot_claim_an_already_claimed_bed() {
        let mut world = ChunkWorld::new(-64, 384);
        world.set_block(100, 71, 205, "minecraft:red_bed[facing=south,occupied=false,part=foot]");

        let mut claims = BedClaims::new();
        let first = find_and_claim_bed(BlockPos::new(100, 70, 202), &world, &mut claims);
        assert_eq!(first, Some(BlockPos::new(100, 71, 205)));

        let second = find_and_claim_bed(BlockPos::new(100, 70, 206), &world, &mut claims);
        assert_eq!(
            second, None,
            "the bed has one ticket and the first villager already holds it"
        );

        // Control: releasing the ticket makes the bed claimable again, so
        // the exclusion above is the occupancy check, not a search miss.
        claims.release(BlockPos::new(100, 71, 205));
        let third = find_and_claim_bed(BlockPos::new(100, 70, 206), &world, &mut claims);
        assert_eq!(
            third,
            Some(BlockPos::new(100, 71, 205)),
            "releasing the ticket must make the bed claimable again"
        );
    }

    /// `validateBedPoi`'s own half this module ports: a bed with a free
    /// ticket is still skipped while its block state reads `occupied=true`.
    #[test]
    fn a_bed_with_someone_already_in_it_is_not_claimable() {
        let mut world = ChunkWorld::new(-64, 384);
        world.set_block(11, 70, 233, "minecraft:blue_bed[facing=north,occupied=true,part=head]");

        let mut claims = BedClaims::new();
        let result = find_and_claim_bed(BlockPos::new(11, 70, 230), &world, &mut claims);
        assert_eq!(
            result, None,
            "a bed currently occupied=true must not be claimable even with a free ticket"
        );

        // Control: the same bed, unoccupied, is claimable — the exclusion
        // above is the `occupied=true` check, not something else about the
        // fixture.
        world.set_block(11, 70, 233, "minecraft:blue_bed[facing=north,occupied=false,part=head]");
        let result = find_and_claim_bed(BlockPos::new(11, 70, 230), &world, &mut claims);
        assert_eq!(result, Some(BlockPos::new(11, 70, 233)));
    }

    #[test]
    fn losing_the_bed_loses_the_claim() {
        let mut claims = BedClaims::new();
        let pos = BlockPos::new(11, 70, 233);
        assert!(claims.try_claim(pos));
        assert!(claims.get(pos).is_some());

        claims.remove(pos);
        assert!(
            claims.get(pos).is_none(),
            "removing the bed must drop its claim, not merely free a ticket"
        );
    }

    /// [`BedClaims::occupied_in_range`] is the primitive issue #241's raid
    /// trigger needs: a claimed bed inside the radius comes back, one
    /// outside does not, and an unclaimed bed (a free ticket, `is_occupied`
    /// false) never does regardless of distance — the same
    /// inside/outside/unoccupied discriminator
    /// `poi_storage::occupied_in_range`'s own test already uses.
    #[test]
    fn occupied_in_range_finds_only_claimed_beds_inside_the_radius() {
        let mut claims = BedClaims::new();
        let center = BlockPos::new(0, 70, 0);

        let inside = BlockPos::new(30, 70, 0);
        let outside = BlockPos::new(70, 70, 0);
        let unclaimed = BlockPos::new(10, 70, 0);

        assert!(claims.try_claim(inside));
        assert!(claims.try_claim(outside));
        claims.discover(unclaimed); // registers the record but claims no ticket

        let found = claims.occupied_in_range(center, 64);
        assert_eq!(
            found,
            vec![inside],
            "only the claimed bed strictly inside the 64-block radius must come back"
        );
    }

    #[test]
    fn a_bell_registers_as_a_meeting_poi_and_nothing_else_does() {
        assert!(is_bell_block("bell"));
        assert!(!is_bell_block("composter"), "a workstation is not a bell");
        assert!(!is_bell_block("white_bed"), "a bed is not a bell");
    }

    /// **The magnitude half, against a bed/workstation's own 1-ticket cap**:
    /// a bell's `PoiTypes.bootstrap` registration is `maxTickets 32`, not `1`
    /// — a test that only checked "a second claim can happen" would pass
    /// with a cap of 2 just as well as 32. Claims 32 tickets successfully at
    /// the same position and asserts the 33rd is refused.
    #[test]
    fn a_bell_holds_exactly_thirty_two_tickets_not_one() {
        let mut claims = BellClaims::new();
        let pos = BlockPos::new(0, 70, 0);
        for n in 1..=32 {
            assert!(claims.try_claim(pos), "ticket {n} of 32 must succeed");
        }
        assert!(
            !claims.try_claim(pos),
            "a 33rd claim must be refused — the cap is 32, not unlimited"
        );

        // Control: releasing one of the 32 frees exactly one slot, not the
        // whole ledger — the same shape `BedClaims::release`'s own test uses.
        claims.release(pos);
        assert!(
            claims.try_claim(pos),
            "releasing one of 32 tickets must make exactly one claim possible again"
        );
        assert!(
            !claims.try_claim(pos),
            "and only one — a second claim right after must still be refused"
        );
    }

    #[test]
    fn a_second_villager_can_still_claim_an_already_claimed_bell() {
        let mut world = ChunkWorld::new(-64, 384);
        world.set_block(100, 71, 205, "minecraft:bell[attachment=floor,facing=south]");

        let mut claims = BellClaims::new();
        let first = find_and_claim_bell(BlockPos::new(100, 70, 202), &world, &mut claims);
        assert_eq!(first, Some(BlockPos::new(100, 71, 205)));

        // Unlike a workstation or a bed (one ticket each), a bell hands out
        // 32 — so a *second* villager searching the same bell must still
        // succeed, the opposite discriminator from
        // `a_second_villager_cannot_claim_an_already_claimed_bed`.
        let second = find_and_claim_bell(BlockPos::new(100, 70, 206), &world, &mut claims);
        assert_eq!(
            second,
            Some(BlockPos::new(100, 71, 205)),
            "a bell has 32 tickets, so a second villager must still be able to claim it"
        );
    }

    #[test]
    fn losing_the_bell_loses_the_claim() {
        let mut claims = BellClaims::new();
        let pos = BlockPos::new(11, 70, 233);
        assert!(claims.try_claim(pos));
        assert!(claims.get(pos).is_some());

        claims.remove(pos);
        assert!(
            claims.get(pos).is_none(),
            "removing the bell must drop its claim, not merely free a ticket"
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
