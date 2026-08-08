# World-save parity against real vanilla 26.2

## What it is

A gate that hands a world to a **real Mojang 26.2 server** in a container, lets vanilla load and save
it, reads it back, and requires semantic identity of every field we author — in both directions.
`crates/lodestone-anvil/tests/vanilla_save_parity.rs`, driven by
`scripts/live-oracles/save-parity.sh`, reporting through `lodestone_anvil::nbt_diff`. It exists because
the owner asked for a roundtrip that proves world saving is 1:1, and because every save test that
existed before it compared our writer against our own reader.

## Why a byte compare is the wrong assertion

A byte-for-byte diff of the two directories **cannot** pass, for four reasons that are all correct
vanilla behaviour. A gate built that way fails for reasons that teach nothing:

| property | consequence for a byte compare |
|---|---|
| `level.dat` legitimately moves — `LastPlayed` is a wall clock, `Time`/`DayTime` advance, `ServerBrands`/`DataPacks`/`Version` are the running server's | always differs |
| vanilla writes `session.lock`, `logs/`, `usercache.json`, and (because this harness force-loads) `data/chunks.dat` | new files appear |
| region chunk payloads are **recompressed** at the writer's own zlib level, and sector placement follows write order rather than content | identical NBT, different `.mca` bytes |
| **NBT compound field order is not part of the value** | two writers agreeing on every field still disagree on bytes |

So the meaningful 1:1 is **semantic identity after canonical decode**. Two layers:

- `lodestone_anvil::nbt_diff` compares NBT trees structurally, matching compounds by field name rather
  than by position, and reports one difference per differing leaf with the **full path** to it —
  `sections[Y=3].block_states.palette[7].Name`, never "chunks differ".
- The gate additionally decodes `block_states` and `biomes` **cell by cell** rather than comparing
  packed `long`s. Canonical NBT is not sufficient there: the `data` array's bit width is a function of
  palette length, so a writer that orders its palette differently packs different `long`s for identical
  blocks. Comparing packed longs measures the palette's order; comparing decoded cells measures the
  world.

Byte identity is **not** achievable and is not a goal. Semantic identity of everything we author is,
and is what the gate asserts.

## The allowlist is the load-bearing part

`ALLOWED` in the test file names each difference a real 26.2 server is expected to introduce, with the
vanilla behaviour that justifies it. An over-broad entry makes the whole gate vacuous — the *assertion*
species from `DESIGN.md` §12.43. The tolerance is the alarm; widening it is cutting the wire.

Every entry is one of exactly two shapes: **a field we deliberately do not write, which vanilla
recomputes from data it does have**, or **a clock, whose purpose is to advance**. There is no entry
touching `block_states`, `biomes`, `Status`, `xPos`/`yPos`/`zPos`, `DataVersion`, `block_entities`,
`block_ticks`, `fluid_ticks` or `structures`, and a non-`#[ignore]`d control asserts that no shipped
pattern can ever reach one of those paths.

Three properties make the list auditable rather than decorative:

- **`Added` and `Removed` are distinguished.** "Vanilla added a field we omit" and "vanilla dropped a
  field we wrote" are the *same NBT path*; an entry that permitted both would silently license data
  loss.
- **`Allow::VanillaOwnsEntirely` has a checked premise.** The light entries permit any change at all,
  which is only sound while we author no light bytes — so direction B asserts our writer emitted zero
  `BlockLight`/`SkyLight` arrays before the handoff. Start writing light and that assertion fails
  first.
- **Every pattern must be reachable from a path the gate really emits.**
  `the_allowlist_matcher_is_tested_against_paths_the_gate_really_emits` builds difference pairs through
  the real comparison functions and requires each pattern to match one. That control exists because its
  absence cost a live run — see the gotchas.

## The two directions, and what each proves

| direction | test | proves |
|---|---|---|
| **A** — we write a fresh world → vanilla loads/saves → we read | `our_fresh_world_survives_a_vanilla_load_and_save` | our **writer** emits nothing vanilla rejects, silently fixes up, or cannot represent — including a `level.dat`/`world_gen_settings.dat` vanilla refuses, which re-rolls the seed and is invisible in the saved blocks |
| **B** — vanilla wrote it → we load → we write → vanilla loads/saves → we read | `a_real_vanilla_world_survives_our_load_and_save` | our **reader** does not drop data it cannot model — the destructive-persistence shape of #477, where a world opened here comes back with its chests emptied |

`crates/lodestone-server/tests/world_persistence_round_trip.rs` is our writer through our reader and
says so in its own header. It is a closed loop — `decode(encode(x)) == x` is satisfied by two symmetric
misunderstandings — and it structurally cannot see anything either direction here sees. Neither replaces
the other.

Direction A's expected value comes from Mojang's server. Direction B's comes from **the same server's
own earlier output**: it compares against what vanilla originally wrote, not against our rewrite.

## How it works

```
scripts/live-oracles/save-parity.sh <serverRoot> <levelName> [rconCommand ...]
```

One-shot, not a long-lived oracle: boot, wait for `Done (`, freeze, force-load, settle, save, stop, wait
for the container to exit. Game port 25590, RCON 25591 — deliberately none of the long-lived oracles'
(25565/6, 25568/9, 25570/1, 25580/1), so it can run alongside them.

`<serverRoot>/<levelName>` is the world. The script writes only `eula.txt`, `server.properties` and
`logs/` into `<serverRoot>` and touches nothing under the world folder — so everything that appears
there afterwards was put there by Mojang's code, which is the point.

It mounts `.cache/mc/26.2` read-only and runs `net.minecraft.server.Main` on an explicit classpath
rather than copying the bundler jar in, the same trick `scripts/anvil-oracle/run.sh` uses: run directly,
`server.jar` extracts ~60 MB of `libraries/` and `versions/` into the working directory on every start.

## Configuration

| knob | where | default | note |
|---|---|---|---|
| `LODESTONE_SAVE_PARITY_SETTLE` | env, read by the script | `10` (seconds) | time for force-loaded chunks to load, light and promote. Lowering it can make the gate vacuous — re-check the `vanilla_rewrote` control if you do |
| `SEED` | test file | `-195_764_831` | direction A's fixed seed. It is the seed of the checked-in real vanilla `tests/support/world_gen_settings_26_2_vanilla.dat`, so that file's seed and its `dimensions` tree come from one real world and need no graft |
| `CHUNKS_A` | test file | `-1..=1` | direction A's 3×3 block, straddling `0` on both axes so it spans four region files and exercises `region_and_local`'s negative-coordinate floor |
| `COMPARED_B` / `WRITTEN_B` | test file | `8` / `10` | direction B compares an 8×8 interior of a written 10×10 block. See the margin gotcha |
| `VANILLA_WORLD_B` | test file | `.cache/mc/survival/world` | not repo state; regenerate with `scripts/live-oracles/survival.sh` |
| JVM heap / VM memory | script | `1200M` / `2g` | lower than the long-lived oracles' `2G`/`3g` on purpose: no players, few chunks, and it usually runs while another oracle already holds 3 GiB on a 16 GB machine |

Both live tests are `#[ignore]`d. Run them explicitly:

```bash
cargo test -p lodestone-anvil --test vanilla_save_parity -- --ignored --nocapture --test-threads=1
```

A **missing** container runtime or fixture is a named panic, never a skip: `#[ignore]` plus
skip-on-absence is the *precondition* species of vacuous test twice over, and this repo already has 277
`#[ignore]` attributes whose rot is unbounded (#536).

## How to change it, and the gotchas

Every one of these was measured while building the gate. Each would have produced a green run that
proved nothing.

- **A server with no players loads almost nothing.** Handed a real world, 26.2 logged `Loading 0
  persistent chunks... / Preparing spawn area: 100% / Time elapsed: 16 ms` and touched no region chunk.
  Hence `forceload add` — and note it takes **block** coordinates and derives the chunk, so
  `forceload add 0 0 31 31` marks four chunks, not thirty-two.
- **A loaded-but-unmodified chunk is not rewritten.** What forces the rewrite is that we write
  `isLightOn = 0`: vanilla relights on load and `ChunkAccess.setLightCorrect(true)` calls
  `markUnsaved()`. That is an inference about vanilla's internals, so it is not trusted — each direction
  carries an `assert_vanilla_rewrote` control requiring the region bytes to have changed **and** a field
  only vanilla writes (`Heightmaps`) to be present in every chunk read back. Without it, "zero
  differences" is indistinguishable from "vanilla never looked".
- **`container logs` is empty for this image, and that is not a transient.** A server that booted fine
  and printed 27 lines to its own `logs/latest.log` produced nothing on `container logs -f`: log4j2's
  console appender is not line-flushed when stdout is not a tty. The readiness poll reads
  `<serverRoot>/logs/latest.log` from the host instead. `logs/` is deleted before boot, because log4j
  rolls an existing `latest.log` into a dated `.gz` *at boot* and a stale file is a `Done (` that has
  already happened.
- **A command vanilla rejects is not a failed RCON call.** `gamerule randomTickSpeed 0` was silently
  refused — 26.2 renamed the rule to `random_tick_speed`
  (`net/minecraft/world/level/gamerules/GameRules.java:74`, with a `GameRuleRegistryFix` for the old
  spelling). The server answered `Incorrect argument for command…<--[HERE]`, which is a perfectly valid
  RCON *response*: the helper printed it and exited 0, the script carried on, and random ticks grew
  kelp through every run while the transcript looked correct. Everything now goes through
  `rcon_checked`, which fails on Brigadier's own error markers. **Do not remove that check to make a
  command work.**
- **Freeze the clock before loading anything.** The world is being loaded and saved, not played, and
  every tick between the two saves is a difference somebody has to allowlist. With the clock running,
  a fragment of a real world produced 60-odd cells of pure noise: kelp and cave vines grew, gravel fell
  (a matched `water[level=0]`→`gravel` / `gravel`→`water[level=0]` pair), pending fluid ticks were
  rescheduled, and two block-entity tick counters moved. `tick freeze` removes all of it, and it is
  safe because `runGameElements = !isFrozen || frozenTicksToRun > 0` gates block, random and entity
  ticks but **not** the chunk source — so loading, promotion and the relight still run.
- **A fragment of a world needs a margin, because vanilla decorates new chunks into existing
  neighbours.** `applyBiomeDecoration` places features over a 3×3 chunk region, so an ore vein rooted
  in a newly generated chunk writes cells into ours. Without the margin, direction B reported 99
  block-state changes that had nothing to do with our save format: 25 `stone → coal_ore`, 8
  `granite → andesite`, a `deepslate → deepslate_diamond_ore`, and a scatter of
  `birch_leaves[distance=N]` updates. A margin removes the cause; allowlisting `block_states` would gut
  the gate. `crates/lodestone-server/tests/decoration_seam_spill.rs` tracks the same mechanism from the
  other side.
- **`WorldGenSettings::from_seed` writes no `dimensions` compound**, so a world built purely from our
  own writers is one vanilla *cannot* open: `WorldGenSettings.CODEC` rejects it and falls back to a
  random seed. Direction A therefore writes the checked-in real vanilla fixture. The failure is
  invisible in the saved blocks — every chunk still loads — so the gate reads vanilla's boot log for
  `Unable to read or access the world gen settings file` and re-reads the seed afterwards.
- **The fixture's contents are a hard precondition, not an assumption.** A fresh ocean or superflat
  world contains no interesting palette and roundtrips trivially — the *world* species, unreadable from
  the test source. `assert_fixture_is_worth_testing` requires a section palette of **more than 16
  entries**, because every palette of 16 or fewer divides 64 evenly and reads correctly under either
  packing rule, so nothing below that threshold exercises the non-spanning rule at all. Direction B
  additionally requires ≥100 block entities across ≥6 kinds, at least one unmodelled; that assertion
  fired at **6** on a hand-picked chunk block and is why the fixture is now chosen by
  `densest_chunk_block` instead.
- **An allowlist pattern written in a path shape the gate never emits matches nothing, and its control
  can pass anyway.** The shipped patterns read `sections[*].SkyLight` while `compare_sections` keys by
  `Y` and emits `sections[Y=-4].SkyLight`; the matcher control asserted against a hand-written
  `sections[7].SkyLight` and passed throughout. The allowlist matched nothing and 41 expected
  differences were reported as failures. **Derive the paths from the comparison, not from the pattern's
  author.**
- **Do not cap the reported difference count.** A summary that truncated would turn "we lost 15,000
  blocks" into "we lost 20". The per-cell list is sampled for readability but the count and the
  bounding box are computed over every cell, so the report can never understate the damage.

## What the gate found on its first runs

Both directions are **red at the time of writing**, and both failures are real defects outside this
crate. The full measurements are in `DESIGN.md` §12.126.

- **Direction A — the generator emits two spellings of one fluid state.** All 153 unallowlisted
  differences across 9 chunks are `minecraft:water` → `minecraft:water[level=0]` (128) or
  `minecraft:lava` → `minecraft:lava[level=0]` (8), in 17 sections. Vanilla's own noise settings carry
  `"Properties": {"level": "0"}` on `default_fluid`;
  `crates/lodestone-worldgen/src/overworld/mod.rs` reads only `["Name"]` and drops it, and hardcodes
  `default_lava` with no properties, while `crates/lodestone-worldgen/src/carver/mod.rs` already uses
  the canonical form. So one column can hold **two palette entries for one block state** — measured at
  4 sections in the direction A fixture.
- **Direction B — 3-D biome data is flattened, and structure references are destroyed.** 696 biome
  cells across 100 sections, every one `minecraft:lush_caves` → the surface biome above it, because
  `ChunkColumn::biome_quarts` is `[String; 16]`: one biome per horizontal quart, constant across `y`.
  And 57 `structures.References`/`starts` entries dropped, because `chunk_nbt` writes an empty stub.

Everything else in direction B is **green, and that is a real result**: zero block-state differences
across 64 real vanilla chunks, zero block entities added or removed out of 145 across 8 kinds (7 of them
unmodelled, surviving only via #477's `BlockEntity::Opaque` passthrough), and `Heightmaps` matching
byte-for-byte — which is independent confirmation of the block preservation, since vanilla recomputed
them from our re-written blocks.

## Dependencies

- `lodestone-anvil`'s own `region`/`level_dat`/`world_gen_settings` (the container and metadata writers
  under test) and the new `nbt_diff` module.
- `lodestone-server` as a **dev-dependency cycle** — `chunk_nbt` (the chunk schema) and
  `overworld_chunk_source` (the generator). The cycle is deliberate and Cargo permits it; the precedent
  is `lodestone-worldgen` and `lodestone-sound`. The alternative was hand-authoring chunk NBT in the
  test, which would assert against the test's own idea of the schema rather than the writer production
  uses.
- Apple `container` and `python3` on PATH, and `.cache/mc/26.2` with the extracted server. See
  [`oracle-runtimes.md`](./oracle-runtimes.md).
- `scripts/live-oracles/rcon-op.py` for RCON framing — vanilla performs exactly one `read()` per
  request, so the whole frame must go out in one write, and that helper already does it.
