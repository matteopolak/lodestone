# Eye of ender

## What it is

Throwing an `ender_eye` into the air sends a small tracked entity toward the nearest stronghold; after 80 ticks it drops back as an item (four throws in five) or shatters. Aiming at an end portal frame is a different action (filling the frame) and keeps priority.

## How it works

Chain, click to pixels:

1. `server::dispatch_play_packet` handles `UseItem` and, for a held `ender_eye`, calls `launch_eye_of_ender` before the generic `apply_use_item` (which has no world source). It does nothing unless the player is in the overworld, is not aiming (within block reach) at an `end_portal_frame`, and `ChunkSource::locate_stronghold` finds a start. Then it consumes one eye (not in creative), spawns the eye through `MobSim::spawn_eye_of_ender`, and publishes the launch sound (neutral, pitch drawn between 0.33 and 0.5) on the block-tick effect lane. The survival slot is resent with a container-slot packet.
2. `locate_stronghold` is a `ChunkSource` method that only the overworld source answers: `OverworldChunkSource` asks the generator for the concentric-ring list of `minecraft:strongholds` (`OverworldGenerator::ring_structure_origins`, 128 chunks, biome-relocated and cached by the registry) and `chunk::nearest_ring_start` picks the nearest by squared distance to each chunk's centre column at y = 32 (the player's height counts). The answer is that chunk's minimum corner at y = 0, which is the point the eye steers at. The list is the placement itself, so it is valid before the stronghold's chunks exist. Wrapper sources (`ChunkStore`, region, dimension, `Arc`, `&S`) forward it.
3. `mobs::eye_of_ender` owns the entity. `signal_target` clamps a target farther than 12 blocks horizontally to a point 12 blocks out on the same bearing and 8 above the launch height. Each tick `tick_eyes` moves the eye by its current velocity and then steers the velocity (`steer`); an eye starts at rest. After its age passes 80 it plays the break sound and is removed.
4. End of life: with the 80% roll made at launch, an `ender_eye` item entity appears at the final position (existing item path); otherwise a level event 2003 (shatter particles) goes to every player. Both ride existing lanes (`take_vocalisations` -> `publish_effect`, and the item stream).
5. Streaming: `MobSim::snapshots` emits the eye as `minecraft:eye_of_ender` with an item-stack metadata field naming `ender_eye`, and the client draws it as a thrown item. The stack field shares metadata index 8 with the item entity, so it is only ever pushed by the eye loop and the item loop.

`tick::run_tick_loop` calls `MobSim::tick_eyes` every tick next to the dragon tick (no world reads are needed: the eye has no collision and no gravity).

## How to change it

- Flight constants (`LIFETIME`, `MAX_REACH`, `RISE`, easing rates) are at the top of `crates/lodestone-server/src/mobs/eye_of_ender.rs`.
- Another structure tag (the placement list is per set id) means passing a different set id from `OverworldChunkSource::locate_stronghold`; other dimensions have no stronghold, so they inherit the trait default `None`.
- Eyes are transient: they are not saved, so a reload drops any in flight, and they stream to clients through the snapshot diff like every other sidecar.
- Gotcha: the locate answer ignores whether the stronghold's chunks are generated or its start is valid; every ring position holds a stronghold because the piece generator retries until it fits.

## Configuration

None. The seed of the drop-or-shatter stream is `EYE_SEED`; the world seed decides where strongholds are.

## Dependencies

- `lodestone_worldgen::structure::StructureRegistry::ring_origins` and the concentric-ring placement (`structure::placement::ring_candidates`).
- The item-entity path (`MobSim::spawn_item`) and the world-effect lane for sounds and level events.
- [`worldgen-structures.md`](./worldgen-structures.md) for how strongholds are built; the portal room's frames get an eye each with probability 0.1, covered by `crates/lodestone-server/tests/eye_of_ender_stronghold.rs`.
