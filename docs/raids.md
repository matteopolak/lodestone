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

**Doors.** Vindicators break doors on Normal and Hard while their raid is active; every raider opens doors during a raid (`ai/door.rs`, rows in `roster/ranged.rs`). Both edit the door through the block-edit seam, so the world owns the change.

**Witch potions.** `witch_potion_for` (`mobs/raid.rs`) picks the throw: a raider target gets healing (4 health or less) or regeneration and is then dropped as a target; anything else gets slowness from 8 blocks, poison at 8+ health, weakness within 3 blocks one time in four (`witch_rng`), otherwise harming. A splash that reaches a player queues timed effects (drained per connection in `play_loop.rs` through `take_player_effects`) and a `PlayerHit` for instant damage.

**Banner on the client.** The captain's banner and mob armour reach clients through the equipment stream, see [`mob-equipment-streaming.md`](./mob-equipment-streaming.md).

## Known gaps

- A raid in progress is saved in the dimension's `EntityRoster` as `RaidRecord`s keyed by raider UUID and restored after the roster (`MobSim::native_raids` / `restore_native_raids`); raiders that no longer exist are dropped. Celebration sounds, enchantment-odds loot and per-player boss-bar gating are not modelled.
- Spawn placement is a coarse ring, not a village-boundary search.

## Configuration

None; the `raids` game rule and difficulty gate raid start.

## Dependencies

`lodestone-entity` (goals, `NavigatingMob`), the POI claim tables in `mobs/`, and the server metadata path (`MetadataField::RaiderCelebrating`, index pinned in `versions/26.2` tests).
