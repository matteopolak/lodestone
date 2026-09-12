# End heightmap status oracle

## What it is

`EndHeightmapStatusOracle` is a bounded external probe for the End generation boundary that affects
cross-chunk feature writes and the three client-visible heightmaps. It runs the real 26.2 server with
seed `42`, holds target chunk `(280,78)` at its pre-feature stage, and records the target before and
after each explicitly requested neighbour reaches the feature stage.

## How it works

The probe requests the target at the pre-feature stage, then admits source chunks one at a time. After
each source reaches the feature stage it reacquires the target, records its persisted stage, dumps raw
heightmap arrays `1`, `4`, and `5`, and prints every block transition in the target's local 16 by 16
band for Y `55..68`. The normal row dump is local Z `0`; `BAND_TRANSITION` lines cover the whole band,
and `FIRST_Y57_WRITE` identifies the first changed block at Y `57` (or `Y57_RESULT` reports that no
such write occurred).

The source orders are deliberately small controls: `target-only`, `canonical-3x3`, `canonical-5x5`,
and `reverse-5x5`. The two five-by-five orders distinguish source admission order from the target's
own stage boundary without materializing a large world. The target's local focus is `(2,0)`; its rows,
heightmap focus values, and complete raw long arrays are emitted in each snapshot.

Run it from the repository root with the external jar cache available:

```bash
LODESTONE_MC_CACHE=/Users/matthew/projects/lodestone/.cache/mc/26.2 \
  scripts/worldgen-oracle/run.sh EndHeightmapStatusOracle --mode canonical-3x3
```

The runner compiles the probe beside `LargeParityOracle` inside a disposable Java 25 container. It
does not read or modify Lodestone generation code.

The validated seed-42 witness is source-order sensitive. In canonical 3×3 and 5×5 order, source
`(280,77)` is the first source that writes the target focus `(2,0)`; it places chorus at Y `63..67`
while the target still reports `CARVERS`. At that point all three maps are already present, with
focus values `67`, `55`, and `55` for types `1`, `4`, and `5`. The target later reaches `FEATURES`
without changing those arrays. In reversed 5×5 order, the target reaches `FEATURES` first; the same
source then writes Y `63..67` while the target reports `FEATURES`, changing only type `1`'s focus from
`57` to `67` while types `4` and `5` remain `57`. The target-only control has no maps before its own
stage and primes all three at focus `57` on entry. No mode produced a changed block at Y `57` in the
captured 16 by 16 band; each run emits `Y57_RESULT first_write=none-in-captured-band`.

## How to change it

Keep the target, Y band, and source-order modes explicit and bounded. If a new witness is needed,
change the constants or add one order to `sources`; keep the full raw arrays and transition output so
the result remains independently inspectable. Re-run the target-only control whenever the server
stage request changes, and compare both five-by-five orders before interpreting an ordering result.

## Configuration

`ORACLE_ARGS` is populated by `run.sh` from the command-line arguments. `ORACLE_TARGET_X` and
`ORACLE_TARGET_Z` optionally override the target chunk, while the seed remains the fixed `42` used by
the runner's server properties. The probe requires the 26.2 cache and Apple `container`.

## Dependencies

The probe uses the real 26.2 server jar and its libraries through `scripts/worldgen-oracle/run.sh`,
the shared `LargeParityOracle` server lifecycle, and the JDK block and chunk APIs exposed by that jar.
It has no dependency on the Rust crates or on a generated fixture.
