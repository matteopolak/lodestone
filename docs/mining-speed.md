# Mining speed

## What it is

Mining speed is the shared break-progress calculation used by the client predictor and
the integrated server's block-break validator. It combines block hardness, held-tool
speed, typed Haste/Conduit Power and Mining Fatigue, and the player's break-speed attributes.

## How it works

`lodestone_game::mining::BreakInputs::dig_speed` applies Efficiency only to a real tool,
then the strongest Haste source, Mining Fatigue, `block_break_speed`, the submerged speed
attribute when the eyes are in water, and the off-ground penalty. `ticks_to_break` replays
the f32 accumulator so exact tick boundaries are preserved. The shell reads the three
break-speed attributes from the local player's server-fed `Attributes` snapshots and
resolves effect roles into `DigSpeedEffects`; unrelated effects are ignored.

The server resolves its block and held-tool censuses into the same `BreakInputs`
calculation before pricing a pending break. Its validator still has a documented
packet-clock tolerance for player state it does not track, so this shared arithmetic
is not by itself a claim of complete anti-cheat parity. Boundary checks count the
start tick inclusively, while deferred completion requires the full accumulated
fraction rather than merely accepting an early stop.

## How to change it

Change the formula or constants in `lodestone_game::mining` and extend its exact-timing
fixtures with inputs where the old and new rules produce different tick counts. Keep the
shell's attribute fold at the single `BreakInputs` construction site. If the server gains
another player-state source, pass it into `progress_per_tick_with_effects` rather than
creating a second formula. Preserve the unrelated-effect control and the Haste amplifier
off-by-one detector when changing effect classification.

## Configuration

There is no runtime configuration. Attribute defaults come from
`lodestone_entity::attribute::default_def`; effect amplitudes are 0-based, so amplifier
zero is level I.

## Dependencies

The calculation depends on `lodestone_game::mining`, `lodestone_data` block/tool censuses,
`lodestone_entity` attribute folding, and the ECS `Attributes`/`HudEffects` components.
The server's pending-break path consumes the shared game calculation and its own
validated effect store.
