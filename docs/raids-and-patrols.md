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

**Not reachable without an edit outside this crate's owned files, or without a wholly
new primitive this crate does not have anywhere** (see §5): **the trigger.**
`MobSim::start_raid` is real, tested, and **has zero production callers** — the same
"code exists, nothing calls it" shape `docs/pillager-patrols.md` itself warns readers
to check for. Nothing in this tree currently:

* grants `minecraft:bad_omen` from an ominous bottle,
* detects a player crossing into a village, or
* converts Bad Omen into `minecraft:raid_omen` on that crossing and starts a raid when
  it expires.

The third item is a small, real hunk (§5). The **second** is not: `poi_storage.rs`'s
own module doc already states *"`PoiManager.isVillageCenter`, not ported here — no
village distance tracker exists in this codebase"* — this is a missing primitive, not
a missing call site, and building it is bigger than this unit's scope. Everything in
§2–4 below is real and works the moment something supplies a raid centre and starts
one; a debug/admin path (a `/raid start` command, say) could exercise it today with no
further work.

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

**Not ported:** `Raid.RaidStatus.LOSS` (losing the village) — needs the same missing
village-distance primitive §1 names, so this port's raids can only reach `Ongoing` or
`Victory`, never a defeat. The 48000-tick (40-minute) overall timeout is ported, so an
abandoned raid does eventually stop being tracked either way.

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

* **The Bad-Omen → Raid-Omen → raid-start trigger**, real but narrow:
  `raid::absorb_raid_omen(existing_level, amplifier)` is the pure arithmetic
  (`Raid.absorbRaidOmen`, clamped to `1..=5`) already written and tested; wiring it
  needs `server.rs`'s per-connection `ActiveEffects` to notice `minecraft:bad_omen`,
  remove it, apply `minecraft:raid_omen` at the new level, and call
  `MobSim::start_raid` when that effect's duration reaches zero. All of that state
  lives in `server.rs`, off limits to this change.
* **Village detection**, real and *not* narrow: nothing in this codebase answers "is
  this position inside a village" (`poi_storage.rs`'s own doc says so). The trigger
  above cannot be completed without it — a village-distance/bell-cluster primitive is
  a prerequisite, not a brokered hunk.
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
* **Wiring the real trigger** is exactly §5's first two bullets, in that dependency
  order (village detection before the omen-expiry call site can mean anything).
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
* `docs/pillager-patrols.md` — the patrol half of #241, and the equipment-slot gap
  this unit's captain marker inherits rather than duplicates.
