# Raids and patrols

## What it is

Issue #241 in two halves. **Patrols already exist and are wired into production** —
see `docs/pillager-patrols.md` for the full account (`MobSim::run_patrol_spawn_cycle`,
`LongDistancePatrolGoal`, wired into `crate::tick`'s production loop). This document
covers the **raid** half: wave-escalating illager assaults with a boss bar and a
captain marker, in `lodestone_server::mobs` (`MobSim::start_raid`/`tick_raids`, in
`mobs/raid.rs`).

---

## 1. What reaches the screen, and what does not yet

**Reaches the screen today, purely as a `lodestone-server` change:** once a raid is
started (`MobSim::start_raid`), it spawns real pillagers and vindicators in escalating
waves matching vanilla's own per-difficulty tables, tracks each wave's survivors by
checking their live health every tick, advances to the next wave on a 300-tick cooldown
once the current one clears, and reaches Victory (with the same 40-tick post-clear
delay vanilla uses) once every wave is spawned and cleared. A real boss bar
(`crate::protocol::BossBarSnapshot`) is published through the **already-existing**
`BOSS_EVENT` wire path — see §3, this needed no protocol change at all. The first
raider of each wave is marked *captain* (`MobSim::raid_captain`), the data-only half of
the ominous-banner marker.

**Not reachable without an edit outside this crate's owned files, or without a
primitive that did not exist until recently** (see §5): **the trigger.**
`MobSim::start_raid` is real, tested, and **has zero production callers** — the same
"code exists, nothing calls it" shape `docs/pillager-patrols.md` itself warns readers
to check for. Nothing in this tree currently:

* grants `minecraft:bad_omen` from an ominous bottle,
* detects a player crossing into a village, or
* converts Bad Omen into `minecraft:raid_omen` on that crossing and starts a raid when
  it expires.

**The real vanilla trigger is narrower than `isVillageCenter` suggests, and this
doc previously named the wrong primitive.** Traced through the decompile:
`RaidOmenMobEffect.applyEffectTick` fires once, on the omen's last tick, and calls
`Raids.createOrExtendRaid(player, raidPosition)` — whose own village check is a
**flat 64-block radius query** (`PoiManager.getInRange`, every occupied
`#village`-tagged POI within 64 blocks, averaged into a raid centre), not
`isVillageCenter`/`sectionsToVillage`'s section-distance-propagation tracker (a
different subsystem this trigger path never touches). `poi_storage.rs`'s
`occupied_in_range` (see `docs/point-of-interest-storage.md`) is that primitive —
built and tested, for the disk-backed `poi/` region set.

**Villager bed-claiming — landed.** `#village` POIs (beds) are now actually
claimed: `crate::mobs::villager::BedClaims`/`find_and_claim_bed`
(`docs/villager-professions-and-trading.md`) is `WorkstationClaims`'s own
shape applied to `PoiTypes.HOME`, wired into the real per-tick `MobSim` loop
via `MobSim::tick_villager_beds`. A villager standing near an unclaimed,
unoccupied bed claims it as a ticket the same tick — no work/rest sleep
cycle needed, since vanilla's own `PoiRecord.isOccupied` (what
`Occupancy.IS_OCCUPIED` reads) is ticket-based, not sleep-based.

**What still blocks a fully disk-backed trigger:** a bed claimed through
`BedClaims` is a **session-only, in-memory** ledger — the same disclosed gap
`WorkstationClaims` already has — so it is never written to the on-disk
`poi/` region set `PoiStorage::occupied_in_range` reads. `MobSim` now also
exposes `occupied_homes_in_range(center, radius)`, the live equivalent of
that disk query scoped to claimed beds, for exactly this reason: **a caller
wiring the real trigger against live villagers should read
`MobSim::occupied_homes_in_range`, not (only) `PoiStorage::occupied_in_range`**,
since the disk query can only ever see a claim that has separately been
persisted, and nothing yet persists one (persisting it touches
`crate::integrated`, off limits to `mobs/villager`). See §5 for the exact
remaining wiring shape, all of it in `server.rs`/`crate::integrated`, outside
this module's ownership.

---

## 2. Wave escalation, transcribed from `Raid.java`

`Raid.getNumGroups` (Peaceful 0 / Easy 3 / Normal 5 / Hard 7) and
`RaiderType.spawnsPerWaveBeforeBonus` for `PILLAGER`
(`[0, 4, 3, 3, 4, 4, 4, 2]`)/`VINDICATOR` (`[0, 0, 2, 0, 1, 4, 2, 5]`), indexed by wave
number, are copied verbatim — real per-wave counts, not guessed or evenly spread. Each
wave's count also rolls `Raid.getPotentialBonusSpawns`'s real difficulty-scaled bonus
(`nextInt(2)` on Easy, a flat `1` on Normal, a flat `2` on Hard, then `nextInt(bonus +
1)`). `mobs/raid.rs`'s own test drives a Hard raid (7 real waves) end to end and asserts
every wave number `1..=7` is reached in order — the discriminating input a single-wave
raid could not show.

**Only pillagers and vindicators spawn.** Ravager/evoker/witch have their own real
`spawnsPerWaveBeforeBonus` arrays in vanilla, but no spawnable species for them exists
in this crate's roster (`docs/plans/villager-economy.md`'s own scope note excludes
them from V10 for the same reason) — not transcribed here rather than silently wrong.

**Not ported:** `Raid.RaidStatus.LOSS` (losing the village) — vanilla's own check is
`isVillageCenter`/`sectionsToVillage`'s section-distance tracker, a genuinely
different subsystem from the trigger's flat radius query (§1) and still unbuilt
here, so this port's raids can only reach `Ongoing` or `Victory`, never a defeat.
The 48000-tick (40-minute) overall timeout is ported, so an abandoned raid does
eventually stop being tracked either way.

---

## 3. The boss bar needed no new wire path at all

`crate::protocol::BossBarSnapshot` and the `BOSS_EVENT` encoder already exist and are
already published every tick (`tick.rs`'s `mob_out.publish_boss_bars(mobs.with(|sim|
sim.boss_bars()))`, built for the dragon and wither fights). `MobSim::boss_bars` (in
`mobs/dragon.rs`, this crate's single public boss-bar entry point) now also calls
`MobSim::push_raid_boss_bars` — one added line, no `tick.rs`/protocol touch needed. The
bar's title mirrors vanilla's own "N raiders remaining" wording at ≤2 survivors;
progress is **wave count**, not vanilla's living-raider health sum (this port does not
track each wave's starting total health) — a disclosed simplification, not a missing
field: `BossBarSnapshot` itself carries no colour/style, the same pre-existing gap the
dragon/wither bars already live with.

---

## 4. The captain marker

The first raider `spawn_wave` places each wave is recorded as that raid's `captain`
(`MobSim::raid_captain`) — the data-only half of vanilla's
`Raid.getOminousBannerInstance` head-slot equipment. **The visual banner itself is the
same pre-existing gap `docs/pillager-patrols.md` §5 already discloses for the patrol
leader**: no mob in this tree carries server-side equipment state at all, so there is
nothing this unit could set even if it owned the wire path.

---

## 5. Known gaps needing work outside this file's ownership

* **`PoiStorage::occupied_in_range` — landed.** The spatial half of the trigger
  (§1's corrected finding) now exists: `poi_storage.rs`'s
  `occupied_in_range(type_predicate, center, radius, occupancy)` is
  `PoiManager.getInRange`, tested against a fixture that separately exercises its
  distance, type and occupancy filters. See `docs/point-of-interest-storage.md`.
* **Villager bed-claiming — landed.** `crate::mobs::villager::BedClaims`/
  `find_and_claim_bed`, wired into production via `MobSim::tick_villager_beds`
  (`docs/villager-professions-and-trading.md`'s own account). A villager
  claims a nearby unoccupied bed's ticket the same tick it finds one; a
  second villager cannot claim an already-held bed; losing the bed (block
  gone or no longer a bed) releases the ticket on the next re-verification.
  **Session-only, like `WorkstationClaims`** — not written to the on-disk
  `poi/` set, so `MobSim::occupied_homes_in_range(center, radius)` is the
  live query the trigger below should use instead of (or alongside)
  `PoiStorage::occupied_in_range`.
* **The Bad-Omen → Raid-Omen → raid-start trigger**, real but narrow, and now
  fully specified: `raid::absorb_raid_omen(existing_level, amplifier)` is the pure
  arithmetic (`Raid.absorbRaidOmen`, clamped to `1..=5`) already written and
  tested. The remaining wiring, now that bed-claiming exists, is mechanical: (1)
  `server.rs`'s per-connection `ActiveEffects` notices `minecraft:bad_omen`
  expiring, removes it, and applies `minecraft:raid_omen` at the new level,
  recording the player's position (`player.getRaidOmenPosition()`'s equivalent);
  (2) on `minecraft:raid_omen`'s own expiry, call a new `MobSim`-side entry point
  that runs `occupied_homes_in_range(recorded_position, 64)` (plus, for parity
  with the wider `#village` tag, `occupied_in_range` over claimed job sites and
  the meeting bell — this module only closes the `home` gap), averages the
  results into a centre, and calls the already-built
  `MobSim::start_raid(center, difficulty, omen_level)` when the query returns
  anything. All of that state lives in `server.rs`.
* **Granting Bad Omen from an ominous bottle.** `item_use.rs` already lists
  `ominous_bottle` among the drinkable-not-food items, but its `Consumable.onConsume`
  effect list (the actual Bad Omen grant) is explicitly unmodelled — the same disclosed
  gap that file's own doc names for milk's status-clear and rotten flesh's hunger
  effect.
* **The ominous banner's visual** — needs the equipment-slot wire path
  `docs/pillager-patrols.md` §5 already asks for; this unit adds a second consumer for
  it (the captain), not a second instance of the gap.
* **Hero of the Village** on victory — `Raid.java`'s own post-victory effect grant to
  players who fought; not built, since it is a small addition once the trigger above
  exists and would otherwise be untestable in isolation.

---

## 6. How to change it

* **Exercising a raid today** without the missing trigger: call `MobSim::start_raid`
  directly (a debug command, a test) — everything downstream (waves, boss bar,
  captain, victory) already runs.
* **Wiring the real trigger** is exactly §5's bullets, in dependency order:
  villager bed-claiming first (so `occupied_in_range` ever finds an occupied
  `#village` POI), then the mechanical `ActiveEffects`/`MobSim` wiring last.
* **Adding ravager/evoker/witch waves** slots in once those species exist in the
  roster: add their own `spawnsPerWaveBeforeBonus` arrays (real data, already known —
  see `Raid.java`'s `RaiderType` enum) next to `PILLAGER_BASE_SPAWNS`/
  `VINDICATOR_BASE_SPAWNS` and a spawn arm in `spawn_wave`.
* **Real health-based boss-bar progress** needs `Raid` to record each wave's starting
  total health (a small field addition) rather than only counting survivors.

---

## 7. Configuration

No new game rule: `raids` (already registered in `game_rules.rs`, `true` by default,
per `docs/pillager-patrols.md`'s own table) is the natural gate once the trigger above
checks it, though nothing in this unit consults it yet (there is no tick-driven trigger
to gate). `mobs/raid.rs`'s `RAID_ROLL_SEED` is the wave-spawn/bonus-count RNG stream's
fixed seed.

---

## 8. Dependencies

* `MobSim::spawn_species` — wave spawning (`mobs/mod.rs`).
* `ChunkWorld::surface_y` — the coarse spawn-ring placement (`wave_spawn_position`).
* `crate::protocol::BossBarSnapshot` and `MobSim::boss_bars` (`mobs/dragon.rs`) — the
  boss-bar publish path, already built for the dragon/wither fights.
* `.cache/mc/26.2/src/net/minecraft/world/entity/raid/{Raid,Raids}.java`,
  `.../world/effect/RaidOmenMobEffect.java`.
* `crate::poi_storage::PoiStorage::occupied_in_range` — the spatial half of the
  real trigger (§1, §5); see `docs/point-of-interest-storage.md`.
* `docs/pillager-patrols.md` — the patrol half of #241, and the equipment-slot gap
  this unit's captain marker inherits rather than duplicates.
