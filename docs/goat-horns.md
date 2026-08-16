# Goat horns

## What it is

The wire half of issue #230's goat work: `Goat.DATA_HAS_LEFT_HORN`/
`DATA_HAS_RIGHT_HORN`, the two entity-metadata fields a client's
`GoatRenderer` reads to hide a broken horn's cuboid. Until this landed,
`MetadataField` had no goat arm at all, so every goat rendered with both
horns regardless of server state — the field this doc covers is what makes
that state exist and reach the wire.

## How it works

`Goat.finalizeSpawn`'s pre-broken-horn roll (`!isBaby() && nextFloat() <
0.1F`, then `nextBoolean()` to pick which horn) is `goat_horn_spawn_roll` in
`crates/lodestone-server/src/mobs/mod.rs`, called from `MobSim::spawn_species`
on its own RNG stream (`goat_horn_rng`, seeded `GOAT_HORN_ROLL_SEED`) before
`entity_type` moves into the mob construction. The result is stored on
`SimMob::has_left_horn`/`has_right_horn` (`true` for every non-goat species,
where the fields are meaningless).

`SimMob::snapshot` pushes `crate::protocol::MetadataField::GoatHorns { has_left,
has_right }` for a `minecraft:goat`, unconditionally (matching `MetadataField::Baby`'s
own "a transition needs the same treatment as the arrival" reasoning, even
though nothing in this crate produces a mid-game transition yet — see
Gotchas). `crates/protocol/v770/src/server_protocol.rs`'s
`encode_set_entity_data` writes it as two `BOOLEAN` fields at indices 19 and
20 (`METADATA_IDX_GOAT_HAS_LEFT_HORN`/`_RIGHT_HORN`), verified against the
committed jar dump (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`).

## How to change it, and the gotchas

- **A horn never breaks mid-game.** `RamTarget`'s own doc (`crates/lodestone-entity/src/brain/behaviors.rs`)
  already discloses that vanilla's `hasRammedHornBreakingBlock` (ramming into
  a block tagged `#minecraft:snaps_goat_horn`) is not ported — that seam has
  no block-state read. So a horn lost here only ever happens at spawn; the
  metadata field itself is real and fully wired, but nothing after spawn ever
  flips it. Wiring the block-contact trigger needs a block-state read
  threaded into the Brain behaviour seam, which does not exist today.
- **Indices 19/20 are two of the most crowded in the game.** Neither is
  self-identifying by serializer alone — see `MetadataField::GoatHorns`'s own
  doc for the full claimant list. The species switch has to live in the
  producer (`SimMob::snapshot`), never in the encoder, for the same reason
  `MetadataField::TamableFlags`/`WitherInvulnerableTicks` already document for
  their own indices.
- **`SpawnRng` is not a bit-identical port of `java.util.Random`.** The 10%
  roll and the "which horn" coin flip land close to vanilla's real
  distribution, not byte-for-byte the same sequence — the same disclosed
  approximation `mobs::raid::bonus_spawns` already carries for its own rolls.
- **Screaming-goat (`DATA_IS_SCREAMING_GOAT`, index 18) is a separate,
  still-unwired field.** This doc's scope is horns only; do not assume the
  screaming variant reaches the wire because horns do.

## Configuration

None — no flags or game rules gate this. `GOAT_HORN_ROLL_SEED` is the roll's
fixed stream seed (`mobs/mod.rs`).

## Dependencies

- `MobSim::spawn_species` (`crates/lodestone-server/src/mobs/mod.rs`) — the
  spawn-time roll.
- `crate::protocol::MetadataField` (`crates/lodestone-server/src/protocol.rs`)
  — the version-free field.
- `crates/protocol/v770/src/server_protocol.rs` — the wire encoder.
- `crates/protocol/v770/tests/support/entity_data_index_jvm.txt` — the
  `EntityDataIndexOracle` dump the indices are verified against.
- `.cache/mc/26.2/src/net/minecraft/world/entity/animal/goat/Goat.java`.

## Verification

```bash
cargo test -p lodestone-server --lib --no-fail-fast -- mobs::goat_horn_tests::
cargo test -p lodestone-v770 --lib --no-fail-fast -- goat_horns
```
