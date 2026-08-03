# Block sound types

## What it is

`lodestone_data::sound_types` — the per-block-state `SoundType` census: break,
step, place, hit and fall sound for every one of the 32,366 block states, plus
the type's volume and pitch. It is the data half of every surface sound a block
makes, and its absence is the whole reason block breaking was silent (see
[sound playback](./sound-playback.md)).

The packet that plays a break sound, `LEVEL_EVENT` 2001, carries **a block-state
id and nothing else**. Vanilla's `LevelEventHandler` `case 2001` looks the sound
up locally from `Block.stateById(data).getSoundType()`
(`LevelEventHandler.java:283-291`). A client without this table can decode and
route that packet perfectly and still have nothing to play.

## How it works

```
crates/lodestone-data/
  oracle-java/SoundTypeOracle.java       # boots the real 26.2 server, dumps
  tests/support/sound_types_jvm.txt      # the committed dump (the anchor)
  tests/sound_types.rs                   # generate-or-assert + the gates
  src/generated/sound_types.rs           # ENTRIES + STATE_ENTRY (generated)
  src/sound_types.rs                     # the accessors
```

### The oracle

`SoundTypeOracle` walks `Block.BLOCK_STATE_REGISTRY` and reads
`BlockStateBase.getSoundType()` per state. No `BlockGetter`, no `BlockPos` and no
data pack are needed — unlike `ShadeBrightnessOracle`, `getSoundType` takes only
the state, reads no tag and touches no level.

It emits six line kinds:

| kind | meaning |
|---|---|
| `C <states> <blocks> <distinctValues> <distinctIdentities>` | the two distinct counts |
| `N <registryId> <name>` | every sound event any row references, from the **live** registry |
| `T <index> <volBits> <pitchBits> <break> <step> <place> <hit> <fall>` | the deduplicated table |
| `O <block> <class>` | the `getSoundType(BlockState)` override census, by reflection |
| `B <firstStateId> <block>` | block ranges |
| `R <index> <runLength>` | run-length encoding of the per-state index |

Byte-reproducible: two runs produced identical output, md5
`3f79821b53fcba9d9f01a7d71b7f9e86`.

```bash
CACHE="$(cd .cache/mc/26.2 && pwd)"
HERE="$(cd crates/lodestone-data/oracle-java && pwd)"
docker run --rm -v "$CACHE":/mc:ro -v "$HERE":/oracle:ro -w /work eclipse-temurin:25-jdk bash -c '
  CP="/mc/versions/26.2/server-26.2.jar:$(find /mc/libraries -name "*.jar" | tr "\n" ":")"
  cp /oracle/SoundTypeOracle.java /work/ && javac -cp "$CP" -d /work /work/SoundTypeOracle.java
  java -cp "/work:$CP" SoundTypeOracle'
```

### Representation, and the measurement that chose it

Measured on the dump, before choosing anything:

| fact | value |
|---|---|
| block states | 32,366 |
| blocks | 1,196 |
| distinct `SoundType` **values** (seven-tuples) | **126** |
| distinct `SoundType` **objects** | **126** |
| distinct sound events referenced | 582 |
| runs in the per-state index | 376 |
| states with `volume = 1.0, pitch = 1.0` | 124 of the 126 types |

So:

| representation | bytes |
|---|---|
| per-state seven-tuple (`f32, f32, 5×u16`, 20 B padded) | 647,320 |
| 126-entry table + per-state `u8` index | **34,634** |

~19× smaller, and the same shape as [`hardness`](./lodestone-data-crate.md) —
`ENTRIES: [(f32, f32, u16, u16, u16, u16, u16); 126]` plus
`STATE_ENTRY: [u8; 32366]`, pure rodata, O(1), no search.

The two distinct counts being **equal** is what licenses the value-keyed dedup:
if they diverged, the table would be merging two of the game's own sound types.
`value_dedup_collapses_nothing` measures it rather than assuming it.

The five sound columns are `minecraft:sound_event` **registry ids** — the same id
space [`sound_events`](./lodestone-data-crate.md) is indexed by — so no sound name
is duplicated into this table, and
`dump_sound_event_ids_agree_with_the_registries_json_table` cross-checks all 582
referenced ids against a table generated from Mojang's `registries.json` by a
completely different path.

### Why a `u8` index is safe

126 ≤ 255 with room to spare, the generator **panics** rather than truncating if a
version bump pushes past 256, and `there_are_exactly_126_distinct_sound_types`
asserts the count so a new sound type fails loudly instead of being rounded onto a
neighbour by the dedup.

## How to change it

* **After a version bump**: re-dump (command above), then
  ```bash
  LODESTONE_REGEN=1 cargo test -p lodestone-data --test sound_types \
      committed_table_matches_dump -- --ignored --nocapture
  ```
  `committed_entries_match_the_dump` is deliberately **not** `#[ignore]`d: it
  compares the committed table's *values* against the dump rather than the
  generated file's bytes, so a reflow of generated source cannot hide a wrong
  sound id.
* **Adding a consumer**: use `sound_types::break_sound_name(id)` /
  `place_sound_name(id)` / `step_sound_name(id)`. They fold two "nothing to play"
  cases into one `None` — out-of-range id, and the `intentionally_empty` sentinel
  — so a caller does not have to hand-check both. Use
  `sound_types::sound_type(id)` when you need the volume/pitch or a raw id.
* **Scaling**: never retype `(volume + 1) / 2` or `pitch * 0.8`. They are
  `BlockSoundType::break_or_place_volume` / `break_or_place_pitch`, because the
  identical expression appears in **both** vanilla call sites
  (`LevelEventHandler.java:288-289` for the break, `BlockItem.java:87` for the
  placement).

## Gotchas

* **Hand-transcribing `SoundType.java` would have been wrong, and the dump proves
  it in three separate places.**
  * `SoundType.java` declares **127** `public static final SoundType` constants
    and only **126** are reachable from a block state. The dead one is
    `TWISTING_VINES`, the only constant carrying `pitch = 0.5F` — and
    `Blocks.TWISTING_VINES` and its kin pass `SoundType.WEEPING_VINES` instead
    (`Blocks.java:4626-4640`). Pairing constants to blocks by name ships a 0.5
    pitch on twisting vines.
  * `SoundType.IRON` and `SoundType.METAL` are different types and the obvious
    pairing is backwards: `minecraft:iron_block` is `IRON` (`block.iron.*`, pitch
    1.0), while `METAL` (pitch **1.5**) belongs to gold, diamond, emerald and
    redstone blocks, all four rails, hoppers, light weighted pressure plates, and
    turtle/sniffer eggs.
  * Two constants mix and match: `HARD_CROP` is `WOOD`'s break/step/hit/fall with
    `CROP_PLANTED` for placement, and `GLOW_LICHEN` is `GRASS`'s four with
    `VINE_STEP`.
* **Air has a `SoundType`** — `STONE`, as it happens. So "the table answered" is
  not "there is a sound to play". Vanilla's `case 2001` guards with
  `if (!blockState.isAir())`; a consumer must too, or an air-state level event
  plays a stone break. The shell's guard is `sim.rs`'s `is_air_state`.
* **`minecraft:intentionally_empty` is a real registry entry** (`SoundEvents.EMPTY`)
  and appears in the table — `CACTUS_FLOWER`'s step, `DRIED_GHAST`'s place, and
  every slot of `SoundType.EMPTY`. It resolves to no `sounds.json` entry and
  therefore no sample. Exactly three blocks have **no break sound at all**:
  `water`, `lava` and `bubble_column`.
* **The table is state-keyed, not block-keyed, for exactly one block.**
  `DecoratedPotBlock` is the sole `getSoundType(BlockState)` override in the game
  (`DecoratedPotBlock.java:212-214`): its 8 `cracked = true` states break with
  `block.decorated_pot.shatter`, its 8 intact ones with
  `block.decorated_pot.break`. A block-keyed table could not express it, and
  `decorated_pot_is_the_only_per_state_sound_type` measures the claim by
  reflection instead of asserting it.
* **Volume and pitch here are the `SoundType`'s own**, not what vanilla passes to
  the sound manager. See the scaling note above.

## Configuration

None. Pure rodata, no environment variables, no feature flags.

## Dependencies

* `crate::sound_events` — resolves the five `u16` registry ids to
  `minecraft:*` names. This table stores no strings.
* `crate::generated_sound_types` — the generated arrays.
* `crates/lodestone-data/oracle-java/SoundTypeOracle.java` needs `docker` and
  `.cache/mc/26.2` (server jar + libraries) to re-dump. There is no local JDK.
* Consumer: `crates/lodestone-shell/src/sim.rs` (`play_block_break_sound`,
  `play_block_place_sound`) → `ShellAudio` → `lodestone-sound` →
  `lodestone-audio`.
