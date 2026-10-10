# Raids and patrols

## What it is

Pillager patrols wander toward villages and start raids; a raid sends waves of raiders at a village until they are all dead (victory) or the village is gone (loss). The raid state is `lodestone_server::mobs::raid`; raider behaviour is `lodestone_entity::ai::raider` plus the rows in `roster/ranged.rs`.

## How it works

**Village membership.** A village is the claimed points of interest in `bed_claims`, `workstation_claims` and `bell_claims`, listed by `occupied_in_range(center, radius)`. A raid counts as in a village when claimed points lie within 32 blocks of its centre. If none do, the centre moves to the nearest point within 64 blocks. With none there either, the raid is lost when a wave has already spawned, and silently dropped when none has.

**Raid ticking** (`MobSim::tick_raids`). Waves spawn on a ring 20 to 40 blocks from the centre. A lost raid keeps its raiders celebrating for 600 ticks, then is removed. Every 20 ticks, raiders-in-waiting within 96 blocks of an ongoing raid that are not in one join it (`recruit_stray_raiders`). Victory queues Hero of the Village grants at omen level minus one.

**Captain and banner.** The first raider of a wave has `SimMob.wears_banner` and the patrol-leader flag; `equipment_snapshot` reports the ominous banner (`ominous_banner`) in its head slot, and it drops the banner on death when mob drops are on. Raiders of a raid without a captain walk to a dropped banner (`FetchLeaderBannerGoal`) and ask to pick it up within 1.4 blocks; `resolve_banner_pickups` hands it over and makes that raider the captain.

**Raider goals** (`ai/raider.rs`). `RaiderHomeVisitGoal` walks to claimed beds within 48 blocks during a raid. `HoldGroundAttackGoal` makes a patrolling raider attack a visible target and shout, setting every raider within the radius to the same target (`resolve_raider_shouts`). `RaiderCelebrationGoal` runs while the raid is lost and sets the celebrating flag (metadata 16). `HealRaiderTargetGoal` (witch) targets a wounded raider; `witch_potion_for` picks healing at health 4 or below, otherwise regeneration.

## How to change it

- Wave sizes and the bonus roll are in `raid.rs` (`PILLAGER_BASE_SPAWNS`, `VINDICATOR_BASE_SPAWNS`, `bonus_spawns`); ravager, evoker and witch waves are not transcribed.
- A new raider species needs a roster table and an entry in `is_raider_species` (`sim_mob.rs`).
- The village radius, recentre radius, celebration length and recruit range are constants at the top of `raid.rs`.
- Tests: `mobs/tests/raiders.rs` (each has a control) and the unit tests at the bottom of `raid.rs`. In tests `sim.bell_claims.try_claim(BlockPos)` is enough to create a village.

## Known gaps

- The vindicator's door-breaking and door-opening rows stay `Registration::missing`; no door goal exists.
- The server sends no equipment for mobs (no set-equipment packet from `EntityStreamer`, and the item component writer omits `banner_patterns`), so the banner and the celebration pose are not visible on a client. The celebration flag itself is on the wire.
- Raid state is not persisted; celebration sounds, enchantment-odds loot and per-player boss-bar gating are not modelled.
- Spawn placement is a coarse ring, not a village-boundary search.

## Configuration

None; the `raids` game rule and difficulty gate raid start.

## Dependencies

`lodestone-entity` (goals, `NavigatingMob`), the POI claim tables in `mobs/`, and the server metadata path (`MetadataField::RaiderCelebrating`, index pinned in `versions/26.2` tests).
