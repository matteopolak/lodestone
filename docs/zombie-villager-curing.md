# Zombie villager curing (issue #247)

## What it is

The conversion timer between "a golden apple was used on a weakened zombie villager"
and "it is a villager again" — the delay itself, not an instant swap, plus the two
real consequences the issue's own body calls out: a preserved profession/level/xp and
a temporary post-cure "confusion" state (vanilla's Nausea effect).

## How it works

`crates/lodestone-server/src/mobs/villager/conversion.rs` is the pure state machine —
`ConversionState` (starter UUID plus remaining ticks), `roll_conversion_ticks`/
`start_converting` (`random.nextInt(2401) + 3600`, vanilla's 3600–6000-tick range),
`conversion_progress` (`ZombieVillager.getConversionProgress`: normally `1` tick of
progress, occasionally more via a 1% roll that scans nearby iron bars/beds), and
`count_nearby_special_blocks` (the world scan that roll needs, kept lazy — see the
module's own doc for why an eager count would waste a world scan on ~99% of ticks).

`crate::mobs::MobSim` wires it through two real production paths:

- **`MobSim::interact`**'s `zombie_villager` short-circuit: a golden apple on a
  zombie villager without Weakness falls through to the generic dispatch (resolving to
  `Pass`, matching vanilla's own `super.mobInteract`); with Weakness, it starts a real
  `ConversionState`, removes Weakness, adds Strength, and publishes the cure sound
  (`crate::effects::zombie_villager_cure_sound`) through the same
  `MobSim::take_vocalisations` queue `crate::tick::run_tick_loop` drains in production.
  Returns `InteractOutcome::ZombieVillagerConversionStarted` (consumes the item).
- **`MobSim::tick_with_terrain`**'s per-mob loop: every zombie villager with a live
  `ConversionState` gets `conversion_progress` subtracted from its remaining ticks each
  tick. On completion, the mob's `entity_type` flips to `minecraft:villager`,
  `combat_defaults` recomputes its stats, gossip is seeded via
  `villager::reputation::apply_reputation_event(…, ZombieVillagerCured, starter)` (see
  `docs/villager-reputation.md`), Nausea is applied for 200 ticks (the "confusion"
  state), and `crate::effects::SOUND_ZOMBIE_CONVERTED` (`LevelEvent` 1027) is published.

`SimMob::profession`/`villager_level`/`villager_xp` are already generic fields on every
mob (issue #243's own design), so a zombie villager that already carries them needs no
special carry-over code at all — becoming a villager is a field-and-effects flip, not a
data migration.

## How to change it, and the gotchas

- **The countdown only ever advances by `conversion_progress`'s return value, never a
  flat `1`** — dropping the iron-bars/bed acceleration collapses the delay in the
  opposite direction from modelling the cure as instant.
- **`count_nearby_special_blocks` must stay behind `conversion_progress`'s own 1%
  gate** — see `conversion.rs`'s own doc for the world-scan cost this avoids.
- **The no-weakness golden-apple arm is disclosed as `Pass`, not a distinct outcome**
  — vanilla's own `InteractionResult.CONSUME` there does not reduce the item stack
  either, so the *observable* behaviour (nothing happens, item unchanged) matches;
  only the outcome enum's shape differs. See
  `InteractOutcome::ZombieVillagerConversionStarted`'s own doc comment.
- **Adding real difficulty tracking to `MobSim`** would let the Strength amplifier
  (`Math.min(difficulty.getId() - 1, 0)`) be exact rather than the disclosed `0`
  constant this crate currently uses (correct for Easy/Normal/Hard; vanilla's own
  formula goes negative on Peaceful, which no amplifier can express).

## What is not built, named rather than silent

- **No natural zombie-villager spawning** (`Zombie`'s own villager-variant roll on
  hard difficulty) — `natural_spawn.rs`/`mob_spawn.rs`/`spawn_egg.rs` are off limits
  for this change. `minecraft:zombie_villager` is already a registered
  `lodestone_data` entity type and `MobSim::spawn_species` can already produce one
  generically (as a plain hostile zombie, no conversion behaviour) — see
  `conversion.rs`'s own "What is not built" section.
- **No initial random-profession roll** for a naturally spawned zombie villager
  (`initializeZombieVillagerData`) — a converted villager's profession is whatever the
  zombie villager already carried, and nothing here assigns one at zombie-villager
  spawn time.

## Configuration

`CONVERSION_WAIT_MIN`/`CONVERSION_WAIT_MAX`/`MAX_SPECIAL_BLOCKS_COUNT`/
`SPECIAL_BLOCK_RADIUS` in `conversion.rs` are transcribed vanilla constants, not tuned
values. `MobSim::zombie_conversion_rng`'s seed is an arbitrary fixed constant on its own
stream, like every other `*_rng` field on `MobSim`.

## Dependencies

`villager::conversion` depends on `crate::mobs::ChunkWorld` (the block scan) and
`uuid`. `MobSim`'s wiring additionally depends on `crate::effects`
(`zombie_villager_cure_sound`, `SOUND_ZOMBIE_CONVERTED`) and `villager::reputation`
(the `ZombieVillagerCured` gossip seed).

## Evidence

| claim | where |
|---|---|
| the conversion-time roll lands in vanilla's real 3600–6000 range | `mobs/villager/conversion.rs`, `roll_conversion_ticks_uses_the_predicted_range` |
| progress is exactly 1 when the 1% roll misses, and the block count is never even evaluated | `mobs/villager/conversion.rs`, `progress_is_one_when_the_one_percent_roll_misses` |
| progress adds exactly one per successful (< 0.3) block roll, off one shared draw stream | `mobs/villager/conversion.rs`, `progress_adds_one_per_successful_block_roll` |
| the special-block scan caps at 14 rolls, not "all blocks found" | `mobs/villager/conversion.rs`, `nearby_special_blocks_is_capped_at_fourteen_rolls` |
| every bed colour (not just one hardcoded name) counts, with a negative control | `mobs/villager/conversion.rs`, `every_bed_colour_counts_as_a_special_block`, `an_unrelated_block_is_not_a_special_conversion_block` |
| a golden apple on an unweakened zombie villager does nothing (no state, `Pass`) | `mobs/mod.rs`, `a_golden_apple_on_an_unweakened_zombie_villager_does_nothing` |
| a golden apple on a weakened one starts a real `ConversionState`, swaps effects, and reaches the production sound queue | `mobs/mod.rs`, `a_golden_apple_on_a_weakened_zombie_villager_starts_a_real_conversion` |
| a completed conversion (driven through the real `tick()` loop) becomes a real villager, seeds gossip at the predicted value (125), applies Nausea, and reaches `SOUND_ZOMBIE_CONVERTED` on the production queue | `mobs/mod.rs`, `a_completed_conversion_becomes_a_real_villager_with_seeded_gossip` |
