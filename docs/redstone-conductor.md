# Redstone conductor table

## What it is

A per-block-state bit for "conducts redstone power", taken from the real 26.2 server. A bat roosts only under such a block.

## How it works

`oracle-java/RedstoneConductorOracle.java` walks every state of the 26.2 registry and asks the state. The dump is committed as `crates/lodestone-data/tests/support/redstone_conductor_jvm.txt`; an ignored test turns it into `src/generated/redstone_conductor.rs` (one bit per state, packed in `u64` words). `lodestone_data::redstone_conductor::conducts` returns `None` for a state added after 26.2, because the block-state table is an append-only union with later releases and this table captures only the 26.2 prefix. The server falls back to "full collision cube" for those.

## How to change it

`just oracle-redstone-conductor` re-dumps (needs Apple `container`), then `just regen-redstone-conductor` rewrites the generated file. To cover states added after 26.2, the dump has to join the behaviour union (see [data behaviour codegen](./data-behavior-codegen.md)).

## Configuration

`pinned_mc` in the `justfile` names the release dumped.

## Dependencies

`lodestone-data` block-state ids; the server's `mobs::world` roost test is the only reader.
