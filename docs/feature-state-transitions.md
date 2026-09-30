# Feature state transitions

## What it is

Sculk vein placement changes typed block states through numeric boolean offsets. The hot spread and discharge paths avoid rebuilding property sets or scanning every state of the block.

## How it works

`SculkVeinState` represents the seven boolean properties as an offset below `Block::SculkVein.default_state()`. The generated registry contains all 128 combinations contiguously; enabling `down`, `east`, `north`, `south`, `up`, `waterlogged`, and `west` subtracts 64, 32, 16, 8, 4, 2, and 1 respectively from the all-false default.

The spread cursor reads face masks in `SCULK_DIRECTIONS` order: down, up, north, south, west, east. The existing state writer interprets masks as down, east, north, south, up, west. The shortcut preserves these two orders independently. Treating them as inverse operations changes decoration output.

`sculk_face_mask` and `sculk_waterlogged` use the numeric representation for veins and retain the general property path for other blocks. `sculk_vein_state_id` consumes the representation in spread, regrowth, and discharge. These helpers consume no randomness and do not change grid visits or writes.

## How to change it

Change the representation and its controls together if the generated registry layout changes. `sculk_numeric_states_match_registry_properties` checks every captured state against the generated property table, including the boundaries. The asymmetric mask control distinguishes read order from write order, and the non-vein control verifies water and face fallback behavior.

Run the focused controls with `cargo test -p lodestone-worldgen --lib sculk_numeric_states -- --nocapture`. For CPU profiling, inspect stacks beneath `sculk_vein_state_id` and vein calls to `sculk_face_mask`: property reconstruction and block-span resolution should disappear from those paths. Other feature types still legitimately use the general property API.

## Configuration

There are no runtime settings or mutable caches. State layout comes from the generated canonical registry, whose current vein span is 27644 through 27771 inclusive.

## Dependencies

The helpers depend on `lodestone-data` for typed `Block` and `StateId` identities, generated default states, and the general property fallback. Their production consumer is the vegetation sculk patch placer in `lodestone-worldgen`.
