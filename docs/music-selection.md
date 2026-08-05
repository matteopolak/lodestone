# Situational music selection

## What it is

The layer that decides *when* to play a music track and *which* one — menu music,
per-biome overworld music, the creative and underwater variants, and the End boss
and credits tracks. It is pure logic over the player's situation plus a randomised
countdown, and it lives in `crates/lodestone-sound/src/music.rs` with a generated
biome table beside it in `src/biome_music.rs`.

The sound *engine* was never the gap. `SoundRegistry`/`SoundResolver` have always
resolved any named event to a decoded `.ogg`; nothing ever asked for a music event.
This is the thing that asks.

## How it works

Three types and one state machine, all transcribed from decompiled 26.2.

| our type | vanilla | cite |
|---|---|---|
| `music::Music` | `Music` record | `Music.java:8` |
| `music::musics::*` | `Musics` constants | `Musics.java:11-21` |
| `music::BackgroundMusic` | `BackgroundMusic` record | `BackgroundMusic.java:11` |
| `music::MusicFrequency` | `MusicManager.MusicFrequency` | `MusicManager.java:158-196` |
| `music::MusicSituation` | `Minecraft.getSituationalMusic` inputs | `Minecraft.java:2601-2621` |
| `music::MusicManager` | `MusicManager` | `MusicManager.java:18-156` |

### 26.2 does not read music off the biome

This is the restructure most likely to be got wrong from memory, and older
tutorials describe the old shape. There is no `biome.getBackgroundMusic()`.
`Minecraft.getSituationalMusic` asks the camera's **environment-attribute probe**
for `EnvironmentAttributes.BACKGROUND_MUSIC`, registered as
`audio/background_music` (`EnvironmentAttributes.java:94-95`), and a biome
contributes music by *setting that attribute*. Selection order:

1. The open screen's own `getBackgroundMusic()`, if any — wins outright, and also
   forces music volume to `1.0` (`Minecraft.java:2624-2626`).
2. Otherwise, with a player: `END_BOSS` if the dimension is the End *and* the boss
   overlay wants music; else the probed `BackgroundMusic.select(creative, underwater)`,
   which may be `None`.
3. Otherwise (no player — the title screen): `MENU`.

`creative` is **`instabuild && mayfly`** (`Minecraft.java:2615`), not
`gamemode == creative`. A gamemode check gives spectators the creative track.

`BackgroundMusic::select` precedence is **underwater, then creative, then default**
(`BackgroundMusic.java:35-41`), and a specific slot falls back to `default` only
when *absent*. Both halves are easy to invert and the symptom is narrow — the wrong
track while swimming in creative.

### The delay constants

These are load-bearing: wrong values are invisible in a short test and obvious in
play. Every one is asserted in `tests/music_selection.rs`.

| music | min | max | replaces | cite |
|---|---|---|---|---|
| `MENU` | 20 | 600 | yes | `Musics.java:11` |
| `CREATIVE` | 12000 | 24000 | no | `Musics.java:12` |
| `CREDITS` | 0 | 0 | yes | `Musics.java:13` |
| `END_BOSS` | 0 | 0 | yes | `Musics.java:14` |
| `END` | 6000 | 24000 | yes | `Musics.java:15` |
| `UNDER_WATER` | 12000 | 24000 | no | `Musics.java:16` + `:19-21` |
| `GAME` | 12000 | 24000 | no | `Musics.java:17` + `:19-21` |

Note `END`'s min is `FIVE_MINUTES`, not the game tracks' `TEN_MINUTES`; and
`END_BOSS`'s event is `music.dragon` (`SoundEvents.java:1040`), not
`music.end_boss`.

`MusicFrequency` converts minutes to ticks as `minutes * 1200`
(`MusicManager.java:170`) → 24000 / 12000 / 0, then
`getNextSongDelay` (`:174-186`) has three distinct behaviours, two of which are
easy to lose:

- `music == None` → the raw cap, **unrandomised**.
- `Constant` → a flat `STARTING_DELAY` (100), **ignoring** its own cap of 0. Reading
  "0 minutes" literally restarts music every tick.
- otherwise → `Mth.nextInt(rng, min(min_delay, cap), min(max_delay, cap))`,
  **inclusive at both ends** (`Mth.java:146-148`).

A consequence worth knowing: at `Frequent` a game track's range collapses to
`12000..=12000`, so the delay is exactly 12000 and **no random draw is consumed**.

### Two orderings inside the tick that read like bugs

Both are faithful and both are pinned by gates.

1. **A replacing selection consumes two draws and takes the smaller.** Vanilla stops
   the old track and sets `nextSongDelay = nextInt(0, min_delay/2)` (`:49`), but does
   **not** clear `currentMusic`; the next `if` (`:52-55`) therefore sees an inactive
   track and `min`s the delay again with `getNextSongDelay`. Both land in one tick.
   Reordering changes the timing *and* the random stream.
2. **While a track plays, the countdown parks at `max_delay`, not `i32::MAX`.**
   `startPlaying` sets `i32::MAX` (`:81`), but the clamp at `:58` is *outside* the
   `currentMusic != null` block, so from the very next tick it is `min(MAX, max_delay)`.
   It then holds there because the decrement at `:59` is guarded on nothing playing.
   An `assert_eq!(delay, i32::MAX)` a tick later fails against real vanilla — that is
   how this was found.

## How to change it, and the gotchas

### A missing track is silence — and that is a shipped configuration, not a corner

`cargo xtask fetch-sounds` **excludes music by default**: 70 tracks + 22 records,
293.23 MB, only with `--all`. Measured on a normal checkout: **0 of 70** music
objects present. So every track this layer can choose is usually absent.

The degradation is vanilla's own, not a bespoke path. `MusicManager.startPlaying`
assigns `currentMusic` *before* playing and switches on the result
(`MusicManager.java:69-82`): `STARTED_SILENTLY` simply skips the toast. Next tick the
`!isActive` branch clears it and re-arms a normal 12000..=24000 countdown. So
`MusicStart::Silent` recovers through the ordinary "track finished" path — no panic,
no `unwrap`, no blocking wait, and **no busy loop**.

That last clause is the one a weak gate misses, so it is asserted as a *count* of
start attempts over a fixed tick budget (immune to machine load), with a negative
control that makes the absence panic and fails the gate.

**And it is not only about unfetched assets.** Exactly one of 26.2's 54 music events,
`music.nether.warped_forest`, ships with an **empty `sounds` array**, so the warped
forest plays no music even with the full `--all` corpus. Treating an unresolvable
music event as an error would break a vanilla biome. `tests/music_assets.rs` pins
that set as an equality, so if Mojang fills it in we hear about it.

### Music must be streamed, never eagerly decoded

`SoundResolver::resolve_instance` calls `decode_vorbis` and **caches the PCM
forever**. That is right for a footstep and catastrophic for a track:
`music/game/end/the_end.ogg` is 10.76 MiB compressed and **304.33 MiB resident**
decoded, and the eight largest music/record objects are 130–300 MiB each — against a
measured world-layer budget of 77.6 MiB.

Vanilla says so in the data: **all 316 music leaf entries in the real `sounds.json`
declare `"stream": true`** (checked by `tests/music_assets.rs`). `ResolvedSound.stream`
had been parsed by `lodestone-assets` all along and ignored.

Use **`SoundResolver::resolve_streaming`**, added for this, which resolves to a
`StreamingSound { stream: VorbisStream, .. }` without decoding, and returns
`Ok(None)` rather than `DriverError::MissingFile` for absent bytes.

### The biome table is generated, not hand-written

`src/biome_music/table.rs` is produced from
`crates/lodestone-server/assets/worldgen/biome/*.json` — 66 vanilla-derived files
already in this repo, of which 42 carry `minecraft:audio/background_music`. Nothing
read them before: every consumer of `EmbeddedResolver::biome_document` looks only at
`carvers` and `features`, so the whole `attributes` map was parsed by no Rust code.

```bash
LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_music_table
```

`tests/biome_music_table.rs` fails if the committed table has drifted, and *separately*
checks the JSON against `Musics.java`, so a wrong asset dump cannot launder itself
through a regenerated table. The generator exists because the alternative precedent —
`lodestone-assets`' hand-transcribed `BIOME_EFFECTS` in `tint.rs` — has no oracle
outside itself.

**`None` and `Some(EMPTY)` mean different things.** A biome absent from the table adds
nothing and should fall back to `BackgroundMusic::overworld()` (helper:
`overworld_music_for`). `pale_garden` is present-but-empty and means *genuinely no
music* (`OverworldBiomes.java:596`). Collapsing them gives pale garden the overworld
track and silences 24 biomes.

### Nothing in this layer can make a sound

`music.rs` and `biome_music.rs` hold no sink, no device, no clock and no filesystem;
everything audible goes through the caller's `MusicSink`. That is enforced, not just
stated: `the_music_modules_cannot_reach_a_device_or_a_clock` scans both files for
`AudioEngine`, `cpal`, `Instant`, `std::fs` and `Command::new` in code position.

This matters because a test that opens an output device would be *audible on the
owner's machine on every `cargo test`*, and no health check in this repo can see that
— the suite passes. The precedent is real: an accounts-screen unit test here spawned
`Command::new("open")` and opened a Microsoft OAuth URL in the owner's browser on
every `cargo test -p lodestone-shell` run.

## Configuration

- `MusicFrequency` — the "Music Frequency" option. The row already exists in
  `crates/lodestone-shell/src/menu/options.rs:985` but is **inert** (`live: None`),
  as are all eleven `soundSource.*` volume sliders; `config::Options` has no volume
  field at all. Wiring them is not part of this.
- `minecraft:audio/music_volume` — per-biome, default 1.0. Only `pale_garden` sets it
  (to 0.0), which *fades* music out over ~303 ticks rather than cutting it.
- `cargo run -p xtask -- fetch-sounds --all` — required for any music to be audible.

## Dependencies

- `lodestone-audio` — `JavaRandom` (vanilla's `LegacyRandomSource` semantics) and
  `VorbisStream`.
- `lodestone-assets` — `SoundRegistry` resolution, via `SoundResolver`.
- `crates/lodestone-server/assets/worldgen/biome/*.json` — the generated table's
  oracle. A *test-time* path only; the shipped crate embeds plain `const` data and
  needs no JSON parser.

## What is still open

- **No caller yet.** This layer is complete and gated but nothing in
  `lodestone-shell` ticks a `MusicManager`, so it reaches no speakers. That is the
  island risk this repo names as its dominant defect class; see the report on issue
  #135 for the wiring seam (`app/menus.rs::draw_menu` for menu music, and the
  `AudioEngine` ECS resource for in-world music) and why it was left to a shell-owning
  change.
- **Biome attributes over the wire.** On a real vanilla server the attribute is
  `syncable()` and arrives in the biome registry NBT, which
  `protocol/v770/.../registry.rs:531`'s `biome_sky_color` already demonstrates how to
  read. That would let a datapack override music; the generated table would remain the
  singleplayer fallback, since the integrated server sends no registry data at all.
