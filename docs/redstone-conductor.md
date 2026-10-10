# Redstone conductor table

## What it is

A per-block-state bit for "conducts redstone power", taken from the real server of the `mc-version` release. A bat roosts only under such a block.

## How it works

`oracle-java/RedstoneConductorOracle.java` walks every state of the registry and asks the state. The dump is committed as `crates/lodestone-data/tests/support/redstone_conductor_jvm.txt`; an ignored test turns it into `src/generated/redstone_conductor.rs` (one bit per state, packed in `u64` words). `lodestone_data::redstone_conductor::conducts` covers every canonical state; the dump names states and the test joins them to ids.

## How to change it

`just oracle-redstone-conductor` re-dumps (needs Apple `container`), then `just regen-redstone-conductor` rewrites the generated file.

## Configuration

`pinned_mc` in the `justfile` names the release dumped.

## Dependencies

`lodestone-data` block-state ids; the server's `mobs::world` roost test is the only reader.
