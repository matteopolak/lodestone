# World-generation dungeons

## What it is

The `monster_room` configured feature places an underground cobblestone room,
up to two deferred-loot chests, and one monster spawner during Overworld
decoration. Generated block entities travel with the column, so the server's
chunk packet carries their registry records while the save path retains the
deferred loot, occupants, and spawner state needed after reload.

## How it works

The feature draws horizontal radii in the configured feature's random stream,
rejects terrain without a solid ceiling/floor or with the wrong number of shell
openings, then writes the shell from top to bottom. The upper scan row is a
gate only: placement writes from one row below that ceiling check down to the
floor, never overwriting the scan row. Solidity comes from the exact
per-state capability census, so block properties are not collapsed to a
base-name deny-list. Bottom shell blocks use a three-in-four mossy-cobblestone
draw. Each of two chest attempts has three random candidates; a candidate is
accepted only when it is empty and has one solid horizontal neighbour. Its
chest state faces away from that neighbour, and the next random long becomes
the `LootTableSeed` for `minecraft:chests/simple_dungeon`. The final bounded
draw selects skeleton, zombie, zombie, or spider for the spawner. The selected
value is stored as a generated `EntityTypeRef`, not an owned registry-name
string. The enclosing generated sidecar reports its `BlockEntityType`
separately (`mob_spawner`); NBT and protocol names are made only by the server
boundary.

The placement driver retains the feature's raw step/index seed and runs the
normal 3×3 source pass. `VegGrid` records absolute block-entity positions and
the Overworld fold-back filters them to the served chunk, which preserves
spills into receiving columns without a coordinate-specific exception.

The compiled-server fixture at
`crates/lodestone-worldgen/tests/support/monster_room_feature_external.txt`
checks the accepted room's final block digest, material counts, chest loot
seeds, and spawner position. `MonsterRoomFeatureOracle` supplies that fixture;
its sealed-room case is a negative control proving the early gate rejects a
room before any write. All 55 biome documents reachable from the Overworld
parameter table list this placed feature, so the feature is in scope for the
authenticated Overworld parity roots even when a particular seed has no room
inside its 51×51 or 101×101 sample.

## How to change it

The parser arm is in `feature/vegetation/config.rs`; the placement body and
draw order are in `feature/vegetation/dungeon.rs`. Add a new generated entity
variant in `overworld/block_entities.rs` and extend the server bridge in
`lodestone-server/src/chunk_nbt.rs` together so the compiler catches an
unhandled wire form. Keep each generated mob selection in the entity registry
enum, and each generated block-entity id in `BlockEntityType`; do not
reintroduce owned strings between those products and their server boundary.
Keep protected-block checks on materialized wall and
interior writes; preserve the feature's deliberately unchecked lower-wall air
write. Keep random draws adjacent to the successful operation they describe. The
production anchor test in `lodestone-server/src/worldgen_data.rs` is useful
when changing terrain predicates, orientation, or entity metadata.

## Configuration

The configured feature has an empty `config` object. Placement remains data
driven by `placed_feature/monster_room.json` and
`placed_feature/monster_room_deep.json`; the protected-block closure comes
from `#minecraft:features_cannot_replace` in `VegTags`, while the exact
per-state solidity comes from the resolver's block-capability facts.

## Dependencies

The feature uses `VegGrid`, `VegTags`, and the shared `RandomSource` seam. The
server-side conversion uses `GeneratedBlockEntity`, `BlockEntity::Spawner`,
`SpawnerState`, and the chunk NBT encoder. No loot-table roll occurs during
world generation; only the table id and its deterministic seed are carried.
